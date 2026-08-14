//! DET-1.3-style V4 KERNEL PARITY: is the engine's V4 pricing (the V3 concentrated-liquidity
//! kernel applied to V4 state) byte-close to the on-chain Uniswap V4 Quoter, at the SAME state?
//!
//! The engine registers ONLY hookless V4 pools (engine-discovery `factory_v4`: `hooks==address(0)`),
//! so V4's core swap math == V3's and the V3 kernel SHOULD match. This proves it empirically against
//! the real V4Quoter, closing the open question raised by the free-run (candidate stream is
//! V4-dominated; `validated=0` was suspected to be V4 mispricing OR staleness — this isolates it).
//!
//! Pool: canonical ETH/USDC fee-500 hookless V4 pool (currency0 = native ETH = 0x0, currency1 = USDC).
//! The engine reads it via StateView; we compute poolId = keccak(abi.encode(PoolKey)) — StateView
//! returning valid state for that poolId is itself the encoding self-check.
//!
//! Run: `ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example v4_kernel_parity --release`
use alloy_primitives::{keccak256, U256};
use engine_amm::{v3_get_amount_out, TickInfo, V3Slot0};
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use engine_rpc::{fetch_v4_liquidity, fetch_v4_slot0, fetch_v4_tick_window};

const STATE_VIEW: &str = "76fd297e2d437cd7f76d50f01afe6160f86e9990"; // Arbitrum V4 StateView
const V4_QUOTER: &str = "3972c00f7ed4885e145823eb7c655375d275a1c5"; // Arbitrum V4 Quoter
const USDC: &str = "af88d065e77c8cc2239327c5edb3a432268e5831";
// PoolKey for ETH/USDC fee-500: currency0 = native ETH (0x0) < currency1 = USDC.
const FEE: u32 = 500;
const TICK_SPACING: i32 = 10;

fn addr(s: &str) -> [u8; 20] {
    let mut o = [0u8; 20];
    for i in 0..20 {
        o[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    o
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
    b[12..32].copy_from_slice(c0); // word0: currency0
    b[44..64].copy_from_slice(c1); // word1: currency1
    b[93..96].copy_from_slice(&fee.to_be_bytes()[1..4]); // word2: uint24 fee, right-aligned
    let ts = (tick_spacing as u32).to_be_bytes();
    b[125..128].copy_from_slice(&ts[1..4]); // word3: int24 tickSpacing (positive), right-aligned
    b[140..160].copy_from_slice(hooks); // word4: hooks (0x0)
    b
}

/// Encode quoteExactInputSingle(QuoteExactSingleParams{poolKey,zeroForOne,exactAmount,hookData=empty}).
fn encode_quoter_call(pool_key: &[u8; 160], zero_for_one: bool, exact_amount: u128) -> Vec<u8> {
    let sig = "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))";
    let selector = &keccak256(sig.as_bytes())[0..4];
    let mut cd = Vec::with_capacity(4 + 32 + 288);
    cd.extend_from_slice(selector);
    // single dynamic tuple arg → offset 0x20
    let mut w = [0u8; 32];
    w[31] = 0x20;
    cd.extend_from_slice(&w);
    // params head (8 words): poolKey(5) + zeroForOne + exactAmount + hookData offset(0x100)
    cd.extend_from_slice(pool_key); // 5 words
    let mut zfo = [0u8; 32];
    zfo[31] = if zero_for_one { 1 } else { 0 };
    cd.extend_from_slice(&zfo);
    let mut amt = [0u8; 32];
    amt[16..32].copy_from_slice(&exact_amount.to_be_bytes()); // uint128 right-aligned
    cd.extend_from_slice(&amt);
    let mut hoff = [0u8; 32];
    hoff[30] = 0x01; // 0x100 = 256 = params-relative offset to hookData
    cd.extend_from_slice(&hoff);
    // hookData tail: length 0
    cd.extend_from_slice(&[0u8; 32]);
    cd
}

fn kernel_quote(
    slot0: V3Slot0,
    amount_in: u128,
    zero_for_one: bool,
    ticks: &[TickInfo],
) -> Option<u128> {
    let o = v3_get_amount_out(slot0, U256::from(amount_in), zero_for_one, ticks).ok()?;
    o.amount_out.try_into().ok()
}

fn main() {
    let rpc = ArbitrumRpcClient::new(std::env::var("ARBITRUM_RPC_URL").expect("ARBITRUM_RPC_URL"));
    let sv = addr(STATE_VIEW);
    let quoter = addr(V4_QUOTER);
    let eth = [0u8; 20]; // native ETH sentinel
    let usdc = addr(USDC);
    let hooks = [0u8; 20];

    let pk = encode_pool_key(&eth, &usdc, FEE, TICK_SPACING, &hooks);
    let pool_id: [u8; 32] = keccak256(&pk).into();
    println!("# DET-1.3 V4 KERNEL PARITY — ETH/USDC fee-500 hookless V4 pool");
    println!("poolId = 0x{}", faster_hex::hex_string(&pool_id));

    let blk = BlockTag::Latest;
    let slot0 = match fetch_v4_slot0(&rpc, &sv, &pool_id, blk) {
        Ok(s) => s,
        Err(e) => {
            println!("StateView getSlot0 FAILED ({e:?}) — poolId encoding wrong or pool absent");
            return;
        }
    };
    let liq = fetch_v4_liquidity(&rpc, &sv, &pool_id, blk).expect("liquidity");
    println!(
        "StateView OK (encoding self-check PASSED): sqrtP={} tick={} lpFee={} liquidity={}",
        slot0.sqrt_price_x96, slot0.tick, slot0.lp_fee, liq
    );
    let win = fetch_v4_tick_window(&rpc, &sv, &pool_id, slot0.tick, TICK_SPACING, 60, blk)
        .expect("ticks");
    let ticks: Vec<TickInfo> = win
        .iter()
        .map(|t| TickInfo {
            tick: t.tick,
            liquidity_net: t.liquidity_net,
        })
        .collect();
    let k_slot0 = V3Slot0 {
        sqrt_price_x96: slot0.sqrt_price_x96,
        tick: slot0.tick,
        fee_pips: slot0.lp_fee, // engine uses the live lpFee
        liquidity: liq,
        tick_spacing: TICK_SPACING,
    };

    // ETH(18) and USDC(6) — pick sizes that exercise the active range in both directions.
    let eth_sizes: [u128; 4] = [
        1_000_000_000_000_000,
        100_000_000_000_000_000,
        1_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
    ];
    let usdc_sizes: [u128; 4] = [10_000_000, 1_000_000_000, 10_000_000_000, 50_000_000_000];
    let mut worst_bps = 0f64;
    let mut n = 0;
    println!("\ndir            size_in        kernel_out        onchain_out      delta_bps");
    for (zfo, sizes) in [(true, &eth_sizes), (false, &usdc_sizes)] {
        for &amt in sizes.iter() {
            let kout = kernel_quote(k_slot0, amt, zfo, &ticks);
            let cd = encode_quoter_call(&pk, zfo, amt);
            let onchain = match rpc.eth_call(&quoter, &cd, blk) {
                Ok(ret) if ret.len() >= 32 => {
                    let mut b = [0u8; 32];
                    b.copy_from_slice(&ret[0..32]);
                    Some(U256::from_be_bytes(b).try_into().unwrap_or(u128::MAX))
                }
                Ok(_) => None,
                Err(e) => {
                    println!(
                        "{:<5} {:>16}  Quoter eth_call err: {e:?}",
                        if zfo { "0->1" } else { "1->0" },
                        amt
                    );
                    None
                }
            };
            match (kout, onchain) {
                (Some(k), Some(c)) if c > 0 => {
                    let bps = ((k as f64 - c as f64).abs() / c as f64) * 1e4;
                    worst_bps = worst_bps.max(bps);
                    n += 1;
                    println!(
                        "{:<5} {:>16} {:>17} {:>17} {:>13.2}",
                        if zfo { "0->1" } else { "1->0" },
                        amt,
                        k,
                        c,
                        bps
                    );
                }
                _ => println!(
                    "{:<5} {:>16}  kernel={:?} onchain={:?} (skipped)",
                    if zfo { "0->1" } else { "1->0" },
                    amt,
                    kout,
                    onchain
                ),
            }
        }
    }
    println!("\nVERDICT: {n} comparisons, worst delta_bps = {worst_bps:.2}  (DET-1.3 gate: <=100)");
    if n > 0 && worst_bps <= 100.0 {
        println!(
            "V4 KERNEL PARITY: PASS — engine V3-kernel-on-V4-state matches on-chain V4 Quoter."
        );
    } else if n == 0 {
        println!("V4 KERNEL PARITY: INCONCLUSIVE — no successful comparisons.");
    } else {
        println!("V4 KERNEL PARITY: FAIL — engine mis-prices V4 (would generate phantoms).");
    }
}
