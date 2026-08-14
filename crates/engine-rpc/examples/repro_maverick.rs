//! Offline Maverick V2 lens + kernel validation against live on-chain state.
//!
//! Mirrors `repro_dodo.rs`: pulls the real factory roster, filters to the token
//! allowlist, fetches each pool's basic config + lens tick-window, ranks by
//! cached active liquidity, and quotes the most liquid pool. The decisive checks
//! are (1) implied mid-price is sane vs the market and (2) the round-trip
//! A->B->A loses only to fees/slippage (never mints free output — that is the
//! phantom signature). A pool that fail-closes (empty tick window) is reported,
//! not silently skipped.
//!
//! Run: `ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example repro_maverick`

use alloy_primitives::{Address, U256};
use engine_amm::maverick_v2::{MaverickState, DEFAULT_TICK_RADIUS};
use engine_feed::Address as FeedAddress;
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use tiny_keccak::{Hasher, Keccak};

const FACTORY: &str = "0A7e848Aca42d879EF06507Fca0E7b33A0a63c1e";
const LENS: &str = "6A9EB38DE5D349Fe751E0aDb4c0D9D391f94cc8D";

// Token allowlist (symbol, address, decimals).
const ALLOWLIST: &[(&str, &str, u32)] = &[
    ("WETH", "82AF49447D8A07E3BD95BD0D56F35241523FBAB1", 18),
    ("USDC", "AF88D065E77C8CC2239327C5EDB3A432268E5831", 6),
    ("USDt0", "FD086BC7CD5C481DCC9C85EBE478A1C0B69FCBB9", 6),
    ("WBTC", "2F2A2543B76A4166549F7AAB2E75BEF0AEFC5B0F", 8),
    ("ARB", "912CE59144191C1204E64559FE8253A0E49E6548", 18),
    ("wstETH", "5979D7b546E38E414F7E9822514be443A4800529", 18),
    ("DAI", "DA10009cBd5D07dD0CeCc66161FC93D7c9000da1", 18),
    ("weETH", "35751007a407ca6feffe80b3cb397736d2cf4dbe", 18),
    ("rETH", "ec70dcb4a1efa46b8f2d97c310c9c4790ba5ffa8", 18),
    ("ezETH", "2416092f143378750bb29b79ed961ab195cceea5", 18),
    ("sfrxETH", "95ab45875cffdba1e5f451b950bc2e42c0053f39", 18),
    ("GHO", "7dff72693f6a4149b17e7c6314655f6a9f7c8b33", 18),
    ("axlUSDC", "eb466342c4d449bc9f53a865d5cb90586f405215", 6),
];

fn main() {
    let rpc = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL");
    let client = ArbitrumRpcClient::new(&rpc);
    let factory: FeedAddress = hexa(FACTORY);
    let lens: FeedAddress = hexa(LENS);
    let block = BlockTag::Latest;

    // Roster via poolCount() + lookup(start,end).
    let count = match call_u64(&client, &factory, &sel(b"poolCount()"), block) {
        Some(c) => c as usize,
        None => {
            println!("poolCount failed");
            return;
        }
    };
    println!("Maverick factory poolCount = {count}");
    let mut roster: Vec<FeedAddress> = Vec::new();
    let lookup = sel(b"lookup(uint256,uint256)");
    let mut start = 0usize;
    while start < count {
        let end = (start + 100).min(count);
        let mut cd = Vec::with_capacity(68);
        cd.extend_from_slice(&lookup);
        cd.extend_from_slice(&word_u64(start as u64));
        cd.extend_from_slice(&word_u64(end as u64));
        match client.eth_call(&factory, &cd, block) {
            Ok(b) => roster.extend(decode_addr_array(&b)),
            Err(e) => {
                println!("lookup {start}..{end} failed: {e:?}");
                break;
            }
        }
        start = end;
    }
    println!("roster fetched = {}", roster.len());

    let allow: Vec<(FeedAddress, &str, u32)> = ALLOWLIST
        .iter()
        .map(|(s, a, d)| (hexa(a), *s, *d))
        .collect();
    let sym_of = |a: &FeedAddress| -> Option<(&str, u32)> {
        allow
            .iter()
            .find(|(x, _, _)| x == a)
            .map(|(_, s, d)| (*s, *d))
    };

    // Filter to allowlisted pairs, fetch state+lens, rank by cached liquidity.
    let token_a_sel = sel(b"tokenA()");
    let token_b_sel = sel(b"tokenB()");
    struct Cand {
        pool: FeedAddress,
        state: MaverickState,
        sym0: String,
        dec0: u32,
        sym1: String,
        dec1: u32,
        liq_score: f64,
        n_ticks: usize,
    }
    let mut cands: Vec<Cand> = Vec::new();
    let mut allowlisted = 0usize;
    let mut empty_window = 0usize;

    for pool in &roster {
        let ta = match call_addr(&client, pool, &token_a_sel, block) {
            Some(a) => a,
            None => continue,
        };
        let tb = match call_addr(&client, pool, &token_b_sel, block) {
            Some(a) => a,
            None => continue,
        };
        let (s0, d0) = match sym_of(&ta) {
            Some(v) => v,
            None => continue,
        };
        let (s1, d1) = match sym_of(&tb) {
            Some(v) => v,
            None => continue,
        };
        allowlisted += 1;

        let mut state = match engine_rpc::fetch_maverick_state_basic(&client, pool, block, 0) {
            Ok(s) => s,
            Err(e) => {
                println!("  basic fetch {s0}/{s1} 0x{} failed: {e:?}", hx(pool));
                continue;
            }
        };
        match engine_rpc::fetch_maverick_lens(&client, &lens, pool, DEFAULT_TICK_RADIUS, block) {
            Ok(l) => {
                state.ticks = l.ticks;
                state.active_tick = l.active_tick;
                state.sqrt_price_d18 = l.sqrt_price_d18;
                // fees come from fetch_maverick_state_basic; lens gives protocol fee ratio.
                state.protocol_fee_ratio_d3 = l.protocol_fee_ratio_d3;
            }
            Err(e) => {
                println!("  lens {s0}/{s1} 0x{} failed: {e:?}", hx(pool));
                continue;
            }
        }
        if state.ticks.is_empty() {
            empty_window += 1;
        }
        // Liquidity score = sum of d18 reserves across cached ticks (rough TVL proxy).
        let liq_score: f64 = state
            .ticks
            .iter()
            .map(|t| f(t.reserve0_d18, 18) + f(t.reserve1_d18, 18))
            .sum();
        let n_ticks = state.ticks.len();
        cands.push(Cand {
            pool: *pool,
            state,
            sym0: s0.into(),
            dec0: d0,
            sym1: s1.into(),
            dec1: d1,
            liq_score,
            n_ticks,
        });
    }

    println!(
        "allowlisted pools={allowlisted} with-lens={} empty-tick-window={empty_window}",
        cands.len()
    );
    cands.sort_by(|a, b| b.liq_score.partial_cmp(&a.liq_score).unwrap());

    println!("\n=== top pools by cached liquidity (d18-reserve proxy) ===");
    for c in cands.iter().take(8) {
        println!(
            "  {:>7}/{:<7} 0x{}  ticks={:>3} liq~{:.1} active_tick={} sqrtP={}",
            c.sym0,
            c.sym1,
            hx(&c.pool),
            c.n_ticks,
            c.liq_score,
            c.state.active_tick,
            c.state.sqrt_price_d18,
        );
    }

    // Validate the most liquid non-empty pool.
    let top = match cands.iter().find(|c| c.n_ticks > 0 && c.liq_score > 0.0) {
        Some(c) => c,
        None => {
            println!("\nNO pool returned a populated tick window — Maverick lens yields nothing tradable.");
            return;
        }
    };
    println!(
        "\n=== validating {}/{} 0x{} ===",
        top.sym0,
        top.sym1,
        hx(&top.pool)
    );

    let t0: Address = top.state.token0;
    let t1: Address = top.state.token1;

    // Pick a small input: ~0.1 unit of token0 in its native decimals.
    let dx0 = U256::from(10u64).pow(U256::from(top.dec0)) / U256::from(10u64); // 0.1 token0
    match top.state.quote_for_tokens_exact_in(t0, t1, dx0) {
        Ok(q) => {
            let out = f(q.amount_out_native, top.dec1);
            let inp = f(dx0, top.dec0);
            println!(
                "  {:.4} {} -> {:.6} {}  (price {:.6} {}/{}, fee_d18={}, partial={}, ticks_crossed={})",
                inp,
                top.sym0,
                out,
                top.sym1,
                out / inp,
                top.sym1,
                top.sym0,
                q.total_fee_d18,
                q.partial_fill,
                q.ticks_crossed,
            );
            // Round-trip invariant: out token1 back to token0 must lose to fees.
            if !q.amount_out_native.is_zero() {
                match top
                    .state
                    .quote_for_tokens_exact_in(t1, t0, q.amount_out_native)
                {
                    Ok(rt) => {
                        let back = f(rt.amount_out_native, top.dec0);
                        let ratio = back / inp;
                        let verdict = if ratio <= 1.0 {
                            "OK (loses to fees, no phantom)"
                        } else {
                            "PHANTOM — round-trip MINTS value"
                        };
                        println!(
                            "  round-trip {} -> back {:.6} {} (ratio {:.6}) => {}",
                            top.sym0, back, top.sym0, ratio, verdict
                        );
                    }
                    Err(e) => println!("  round-trip reverse quote err: {e:?}"),
                }
            }
        }
        Err(e) => println!("  forward quote err: {e:?}"),
    }
}

fn sel(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..32].copy_from_slice(&v.to_be_bytes());
    w
}

fn call_u64(c: &ArbitrumRpcClient, to: &FeedAddress, data: &[u8], b: BlockTag) -> Option<u64> {
    let r = c.eth_call(to, data, b).ok()?;
    if r.len() < 32 {
        return None;
    }
    Some(u64::from_be_bytes(r[24..32].try_into().ok()?))
}

fn call_addr(
    c: &ArbitrumRpcClient,
    to: &FeedAddress,
    data: &[u8],
    b: BlockTag,
) -> Option<FeedAddress> {
    let r = c.eth_call(to, data, b).ok()?;
    if r.len() < 32 || r[0..12].iter().any(|&x| x != 0) {
        return None;
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&r[12..32]);
    Some(a)
}

fn decode_addr_array(raw: &[u8]) -> Vec<FeedAddress> {
    if raw.len() < 64 {
        return Vec::new();
    }
    let off = u64::from_be_bytes(raw[24..32].try_into().unwrap()) as usize;
    if off + 32 > raw.len() {
        return Vec::new();
    }
    let len = u64::from_be_bytes(raw[off + 24..off + 32].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(len);
    let base = off + 32;
    for i in 0..len {
        let s = base + i * 32;
        if s + 32 > raw.len() {
            break;
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&raw[s + 12..s + 32]);
        out.push(a);
    }
    out
}

fn f(x: U256, dec: u32) -> f64 {
    x.to_string().parse::<f64>().unwrap_or(0.0) / 10f64.powi(dec as i32)
}

fn hexa(s: &str) -> FeedAddress {
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

fn hx(b: &FeedAddress) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
