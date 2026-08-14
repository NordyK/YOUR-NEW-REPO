//! Decisive DODO kernel audit: compare OUR `DodoState` quote against the pool's
//! own on-chain `querySellBase` / `querySellQuote` at the SAME block, for each
//! possible pool class. Isolates whether the ~0.13% phantom is a DODO kernel/class
//! bug (quote diverges at a single consistent block) or a state-consistency bug
//! (quote matches on-chain, so the phantom comes from mixed-block resync state).
//!
//! Run: `ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example dodo_chain_audit`

use alloy_primitives::U256;
use engine_amm::dodo_v2::{DodoPoolClass, DodoState, SwapDirection};
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use tiny_keccak::{Hasher, Keccak};

fn sel(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut o = [0u8; 32];
    k.finalize(&mut o);
    [o[0], o[1], o[2], o[3]]
}

/// eth_call `fn(address trader, uint256 amount)` → first return word as U256.
fn query(
    client: &ArbitrumRpcClient,
    pool: &[u8; 20],
    selector: [u8; 4],
    amount: U256,
    block: BlockTag,
) -> Option<U256> {
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&selector);
    data.extend_from_slice(&[0u8; 32]); // trader = address(0)
    data.extend_from_slice(&amount.to_be_bytes::<32>());
    let resp = client.eth_call(pool, &data, block).ok()?;
    if resp.len() < 32 {
        return None;
    }
    let mut w = [0u8; 32];
    w.copy_from_slice(&resp[0..32]);
    Some(U256::from_be_bytes(w))
}

fn with_class(s: &DodoState, class: DodoPoolClass) -> DodoState {
    let mut c = s.clone();
    c.pool_class = class;
    c
}

fn pct_diff(ours: U256, chain: U256) -> f64 {
    if chain.is_zero() {
        return f64::NAN;
    }
    let o = ours.to_string().parse::<f64>().unwrap_or(0.0);
    let c = chain.to_string().parse::<f64>().unwrap_or(0.0);
    (o - c) / c * 100.0
}

fn main() {
    let rpc = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL");
    let client = ArbitrumRpcClient::new(&rpc);

    // DODO WBTC(base)/USDC(quote) — the middle hop in ALL 11 phantom-detected routes.
    let pool: [u8; 20] = hexa("0d32a3bb04e8ff29f0974c9d3f3d85012a0b5d14");
    let bn = client.block_number().expect("block_number");
    let block = BlockTag::Number(bn);
    println!("block = {bn}");

    // Fetch once (class only affects effective_pmm_state, not the raw reads).
    let s = match engine_rpc::fetch_dodo_state(&client, &pool, DodoPoolClass::Dvm, block, bn) {
        Ok(s) => s,
        Err(e) => {
            println!("fetch failed: {e:?}");
            return;
        }
    };
    println!(
        "base={} quote={} i={} K={} B={} Q={} B0={} Q0={} R={:?} fee_ppm={}",
        hx(s.base_token.as_slice()),
        hx(s.quote_token.as_slice()),
        s.i,
        s.k,
        s.base_reserve,
        s.quote_reserve,
        s.base_target,
        s.quote_target,
        s.r_state,
        s.fee_ppm
    );

    let qsb = sel(b"querySellBase(address,uint256)");
    let qsq = sel(b"querySellQuote(address,uint256)");

    // WBTC is 8-dec; test a spread of base amounts (0.001 .. 0.5 WBTC).
    let base_amts = [100_000u128, 1_000_000, 10_000_000, 50_000_000];
    println!("\n=== sellBase (WBTC -> USDC): our amount_out vs on-chain querySellBase ===");
    for amt in base_amts {
        let a = U256::from(amt);
        let chain = query(&client, &pool, qsb, a, block);
        print!(
            "WBTC_in={amt:>10}  chain_out={}",
            chain.map(|x| x.to_string()).unwrap_or("ERR".into())
        );
        for class in [DodoPoolClass::Dvm, DodoPoolClass::Dpp, DodoPoolClass::Dsp] {
            let st = with_class(&s, class);
            match st.quote_exact_in_local(a, SwapDirection::BaseToQuote) {
                Ok(q) => {
                    let d = chain
                        .map(|c| pct_diff(q.amount_out_native, c))
                        .unwrap_or(f64::NAN);
                    print!("  | {:?} out={} ({:+.4}%)", class, q.amount_out_native, d);
                }
                Err(e) => print!("  | {:?} ERR {:?}", class, e),
            }
        }
        println!();
    }

    // USDC is 6-dec; test a spread of quote amounts (10 .. 50k USDC).
    let quote_amts = [10_000_000u128, 100_000_000, 1_000_000_000, 50_000_000_000];
    println!("\n=== sellQuote (USDC -> WBTC): our amount_out vs on-chain querySellQuote ===");
    for amt in quote_amts {
        let a = U256::from(amt);
        let chain = query(&client, &pool, qsq, a, block);
        print!(
            "USDC_in={amt:>12}  chain_out={}",
            chain.map(|x| x.to_string()).unwrap_or("ERR".into())
        );
        for class in [DodoPoolClass::Dvm, DodoPoolClass::Dpp, DodoPoolClass::Dsp] {
            let st = with_class(&s, class);
            match st.quote_exact_in_local(a, SwapDirection::QuoteToBase) {
                Ok(q) => {
                    let d = chain
                        .map(|c| pct_diff(q.amount_out_native, c))
                        .unwrap_or(f64::NAN);
                    print!("  | {:?} out={} ({:+.4}%)", class, q.amount_out_native, d);
                }
                Err(e) => print!("  | {:?} ERR {:?}", class, e),
            }
        }
        println!();
    }
}

fn hexa(s: &str) -> [u8; 20] {
    let mut o = [0u8; 20];
    for i in 0..20 {
        o[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    o
}
fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
