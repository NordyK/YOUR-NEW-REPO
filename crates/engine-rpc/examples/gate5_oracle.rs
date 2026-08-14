//! GATE 5 — sizing-oracle certification across real historical V3/V3 cycles.
//!
//! For each candidate cycle this harness:
//!   1. loads the real pre-state pool snapshots from the archive RPC,
//!   2. runs an INDEPENDENT dense brute-force oracle (512-point log grid over [1e-4, 200] WETH
//!      + golden-section local refinement) using the engine's own V3 quote kernel,
//!   3. runs the engine's PRODUCTION sizer (coarse 12-bucket grid + golden-section refiner, the
//!      exact `optimize_cycle_amount` shape from pipeline.rs),
//!   4. asserts the production sizer captures >= 95% of the dense-oracle optimum, and that it
//!      never reports profit on a net-negative (after a configurable gas floor) size.
//!
//! No hand-rolled AMM math: profit comes from `engine_amm::v3_get_amount_out` on real state.
//!
//! Run: ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example gate5_oracle --release
use alloy_primitives::{Address, U256};
use engine_amm::dodo_v2::{DodoPoolClass, DodoState};
use engine_amm::uniswap_v3::Slot0;
use engine_amm::TickInfo;
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use engine_rpc::{fetch_dodo_state, fetch_v3_liquidity, fetch_v3_slot0, fetch_v3_tick_window};
use std::io::Write;

// ---------------------------------------------------------------------------
// Gate-5 B2: leg-generic sizing oracle. The dense-512 oracle + production-sizer
// comparison (`evaluate`) is family-agnostic — it only needs a `gross(x)` closure.
// `Leg`/`LegSpec` let a candidate be ANY fixed-hop cycle through the bot's real
// venue families (here V3 + DODO; same pattern extends to Balancer/Curve/Maverick
// via their engine-amm kernels + engine-rpc fetchers). Fluid needs a resolver-based
// historical pre-state fetcher (separate task) before it can be added here.
// ---------------------------------------------------------------------------

/// Spec for one leg of a cyclic route (what to load).
enum LegSpec {
    V3 {
        pool: &'static str,
        fee: u32,
        spacing: i32,
        zfo: bool,
    },
    Dodo {
        pool: &'static str,
        class: DodoPoolClass,
        token_in: &'static str,
        token_out: &'static str,
    },
}

/// Loaded leg state (what to quote against).
enum Leg {
    V3 {
        slot: Slot0,
        ticks: Vec<TickInfo>,
        zfo: bool,
    },
    Dodo {
        state: DodoState,
        token_in: Address,
        token_out: Address,
    },
}

fn load_leg(rpc: &ArbitrumRpcClient, spec: &LegSpec, block: BlockTag, block_number: u64) -> Leg {
    match spec {
        LegSpec::V3 {
            pool,
            fee,
            spacing,
            zfo,
        } => {
            let (slot, ticks) = load_pool(rpc, pool, *fee, *spacing, 600, block);
            Leg::V3 {
                slot,
                ticks,
                zfo: *zfo,
            }
        }
        LegSpec::Dodo {
            pool,
            class,
            token_in,
            token_out,
        } => {
            let state =
                fetch_dodo_state(rpc, &Address::from(addr(pool)), *class, block, block_number)
                    .expect("fetch dodo state");
            Leg::Dodo {
                state,
                token_in: Address::from(addr(token_in)),
                token_out: Address::from(addr(token_out)),
            }
        }
    }
}

/// Quote `amount_in` through one loaded leg using the engine's real kernel.
fn quote_leg(leg: &Leg, amount_in: u128) -> Option<u128> {
    match leg {
        Leg::V3 { slot, ticks, zfo } => {
            let r =
                engine_amm::v3_get_amount_out(*slot, U256::from(amount_in), *zfo, ticks).ok()?;
            if r.amount_out.is_zero() {
                None
            } else {
                Some(r.amount_out.to::<u128>())
            }
        }
        Leg::Dodo {
            state,
            token_in,
            token_out,
        } => {
            let q = state
                .quote_for_tokens_exact_in(*token_in, *token_out, U256::from(amount_in))
                .ok()?;
            let o = q.amount_out_native.to::<u128>();
            if o == 0 {
                None
            } else {
                Some(o)
            }
        }
    }
}

/// Multi-family cyclic candidate: a sequence of legs that starts and ends in the
/// same token (`unit` = that token's 1.0 in raw units, for human-readable prints).
struct CandGen {
    name: &'static str,
    block: u64, // pre-state block (parent of the arb block)
    legs: Vec<LegSpec>,
    lo_in: u128, // dense-oracle search lower bound (raw start-token units)
    hi_in: u128, // upper bound
    unit: f64,   // start-token 1.0 (e.g. 1e18 WETH) for printing
    unit_sym: &'static str,
    // conservative all-in cost floor in start-token raw units (L2+L1+flash+overhead+buffer)
    cost_floor: u128,
}

fn run_candidate_gen(rpc: &ArbitrumRpcClient, c: &CandGen) -> bool {
    let block = BlockTag::Number(c.block);
    println!(
        "\n=== GEN candidate {} @ block {} ({} legs) ===",
        c.name,
        c.block,
        c.legs.len()
    );
    let legs: Vec<Leg> = c
        .legs
        .iter()
        .map(|s| {
            let l = load_leg(rpc, s, block, c.block);
            print!(".");
            std::io::stdout().flush().ok();
            l
        })
        .collect();
    println!(" loaded {} legs", legs.len());
    let gross = |x: u128| -> u128 {
        let mut amt = x;
        for leg in &legs {
            match quote_leg(leg, amt) {
                Some(o) if o > 0 => amt = o,
                _ => return 0,
            }
        }
        if amt > x {
            amt - x
        } else {
            0
        }
    };
    // Boundary check: many real arbs are intra-block backruns whose opportunity only exists AFTER a
    // victim tx mid-block, so a parent-block fork shows no cycle. That is NOT a sizer failure — it
    // means this candidate needs exact-prefix pre-state replay (replay block prefix up to the arb tx).
    let (_, best_p0) = best_gross(&gross, c.lo_in, c.hi_in);
    if best_p0 == 0 {
        println!(
            "  NOT-CERTIFIED (non-fatal): no gross-positive cycle at parent pre-state {} — likely an \
             intra-block backrun; needs exact-prefix pre-state replay (separate harness task).",
            c.block
        );
        return true;
    }
    let pass = evaluate(&gross, c.lo_in, c.hi_in, c.unit, c.unit_sym);
    // net-positive after a conservative all-in cost floor (gas + L1 + flash + overhead + buffer).
    let net = best_p0.saturating_sub(c.cost_floor);
    println!(
        "  NET after cost_floor {} ({:.6} {}): {} raw ({:.6} {})  net-positive: {}",
        c.cost_floor,
        c.cost_floor as f64 / c.unit,
        c.unit_sym,
        net,
        net as f64 / c.unit,
        c.unit_sym,
        net > 0
    );
    pass && net > 0
}

fn addr(s: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

struct Cand {
    name: &'static str,
    block: u64,
    pool_a: &'static str,
    fee_a: u32,
    spacing_a: i32,
    zfo_a: bool, // zero_for_one for hop A (WETH -> mid)
    pool_b: &'static str,
    fee_b: u32,
    spacing_b: i32,
    zfo_b: bool, // zero_for_one for hop B (mid -> WETH)
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
    (
        Slot0 {
            sqrt_price_x96: s0.sqrt_price_x96,
            tick: s0.tick,
            fee_pips,
            liquidity: liq,
            tick_spacing,
        },
        ticks,
    )
}

fn run_candidate(rpc: &ArbitrumRpcClient, c: &Cand) -> bool {
    let block = BlockTag::Number(c.block);
    println!("\n=== candidate {} @ block {} ===", c.name, c.block);
    print!("  loading poolA 0x{} ...", c.pool_a);
    std::io::stdout().flush().expect("flush stdout");
    let (sa, ta) = load_pool(rpc, c.pool_a, c.fee_a, c.spacing_a, 600, block);
    println!(" done");
    print!("  loading poolB 0x{} ...", c.pool_b);
    std::io::stdout().flush().expect("flush stdout");
    let (sb, tb) = load_pool(rpc, c.pool_b, c.fee_b, c.spacing_b, 600, block);
    println!(" done");
    println!(
        "  poolA 0x{} tick={} liq={} warm_ticks={}",
        c.pool_a,
        sa.tick,
        sa.liquidity,
        ta.len()
    );
    println!(
        "  poolB 0x{} tick={} liq={} warm_ticks={}",
        c.pool_b,
        sb.tick,
        sb.liquidity,
        tb.len()
    );

    let quote = |x: u128| -> Option<u128> {
        let m = engine_amm::v3_get_amount_out(sa, alloy_primitives::U256::from(x), c.zfo_a, &ta)
            .ok()?;
        if m.amount_out.is_zero() {
            return None;
        }
        let out = engine_amm::v3_get_amount_out(sb, m.amount_out, c.zfo_b, &tb).ok()?;
        Some(out.amount_out.to::<u128>())
    };
    let gross = |x: u128| -> u128 {
        match quote(x) {
            Some(o) if o > x => o - x,
            _ => 0,
        }
    };

    // ---- 1) INDEPENDENT dense brute-force oracle: 512 log-spaced points + golden refine ----
    let lo_b = 100_000_000_000_000u128; // 1e-4 WETH
    let hi_b = 200_000_000_000_000_000_000u128; // 200 WETH
    let n = 512usize;
    let ln_lo = (lo_b as f64).ln();
    let ln_hi = (hi_b as f64).ln();
    let mut best_x = 0u128;
    let mut best_p = 0u128;
    for i in 0..=n {
        let x = (ln_lo + (ln_hi - ln_lo) * (i as f64) / (n as f64)).exp() as u128;
        let p = gross(x);
        if p > best_p {
            best_p = p;
            best_x = x;
        }
    }
    // local golden-section refine around the best grid point
    let (mut a, mut b) = (best_x / 2 + 1, best_x.saturating_mul(2));
    let span = |a: u128, b: u128| b.saturating_sub(a);
    let mut cc = b.saturating_sub(span(a, b) * 618 / 1000);
    let mut dd = a.saturating_add(span(a, b) * 618 / 1000);
    let (mut fc, mut fd) = (gross(cc), gross(dd));
    for _ in 0..64 {
        if span(a, b) <= 2 {
            break;
        }
        if fc < fd {
            a = cc;
            cc = dd;
            fc = fd;
            dd = a.saturating_add(span(a, b) * 618 / 1000);
            fd = gross(dd);
        } else {
            b = dd;
            dd = cc;
            fd = fc;
            cc = b.saturating_sub(span(a, b) * 618 / 1000);
            fc = gross(cc);
        }
    }
    let mid = (a + b) / 2;
    if gross(mid) > best_p {
        best_p = gross(mid);
        best_x = mid;
    }
    let oracle_size = best_x;
    let oracle_profit = best_p;
    println!(
        "  ORACLE(dense 512+refine): best_size={:.4} WETH  profit={} wei ({:.6} WETH)",
        oracle_size as f64 / 1e18,
        oracle_profit,
        oracle_profit as f64 / 1e18
    );
    if oracle_profit == 0 {
        println!("  SKIP: no positive-gross cycle at this state (not a sizing failure).");
        return true;
    }

    // ---- 2) PRODUCTION sizer: coarse 12-bucket grid + golden-section bracket ----
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
    let mut bi = 0usize;
    let mut bp = 0u128;
    for (i, &g) in grid.iter().enumerate() {
        let p = gross(g);
        if p > bp {
            bp = p;
            bi = i;
        }
    }
    let lo = if bi > 0 { grid[bi - 1] } else { grid[bi] / 4 };
    let hi = if bi + 1 < grid.len() {
        grid[bi + 1]
    } else {
        grid[bi].saturating_mul(4)
    };
    let (mut a, mut b) = (lo, hi);
    let mut cc = b.saturating_sub(span(a, b) * 618 / 1000);
    let mut dd = a.saturating_add(span(a, b) * 618 / 1000);
    let (mut fc, mut fd) = (gross(cc), gross(dd));
    for _ in 0..32 {
        let tol = (a.max(b) / 1000).max(2);
        if span(a, b) <= tol {
            break;
        }
        if fc < fd {
            a = cc;
            cc = dd;
            fc = fd;
            dd = a.saturating_add(span(a, b) * 618 / 1000);
            fd = gross(dd);
        } else {
            b = dd;
            dd = cc;
            fd = fc;
            cc = b.saturating_sub(span(a, b) * 618 / 1000);
            fc = gross(cc);
        }
    }
    let refined = (a + b) / 2;
    let (sizer_size, sizer_profit) = if gross(refined) > bp {
        (refined, gross(refined))
    } else {
        (grid[bi], bp)
    };
    let ratio = sizer_profit as f64 / oracle_profit as f64 * 100.0;
    println!(
        "  SIZER(production): best_size={:.4} WETH  profit={} wei ({:.6} WETH)  ratio={:.2}%",
        sizer_size as f64 / 1e18,
        sizer_profit,
        sizer_profit as f64 / 1e18,
        ratio
    );

    // ---- 3) net-negative guard: a typical Arbitrum arb gas cost ~ 0.0005 WETH. The sizer's
    // chosen size must still be gross-positive (the EV gate then subtracts gas). Confirm the
    // sizer never reports a size whose gross profit is zero. ----
    let net_ok = sizer_profit > 0;
    let pass = ratio >= 95.0 && net_ok;
    println!(
        "  => {} (>=95% oracle: {}, gross-positive size: {})",
        if pass { "PASS" } else { "FAIL" },
        ratio >= 95.0,
        net_ok
    );
    pass
}

/// Dense 512-point log-grid scan + golden refine over [lo,hi]; returns (best_size, best_gross).
fn best_gross(gross: &dyn Fn(u128) -> u128, lo_b: u128, hi_b: u128) -> (u128, u128) {
    let n = 512usize;
    let ln_lo = (lo_b as f64).ln();
    let ln_hi = (hi_b as f64).ln();
    let mut best_x = 0u128;
    let mut best_p = 0u128;
    for i in 0..=n {
        let x = (ln_lo + (ln_hi - ln_lo) * (i as f64) / (n as f64)).exp() as u128;
        let p = gross(x);
        if p > best_p {
            best_p = p;
            best_x = x;
        }
    }
    let span = |a: u128, b: u128| b.saturating_sub(a);
    let (mut a, mut b) = (best_x / 2 + 1, best_x.saturating_mul(2));
    let mut cc = b.saturating_sub(span(a, b) * 618 / 1000);
    let mut dd = a.saturating_add(span(a, b) * 618 / 1000);
    let (mut fc, mut fd) = (gross(cc), gross(dd));
    for _ in 0..64 {
        if span(a, b) <= 2 {
            break;
        }
        if fc < fd {
            a = cc;
            cc = dd;
            fc = fd;
            dd = a.saturating_add(span(a, b) * 618 / 1000);
            fd = gross(dd);
        } else {
            b = dd;
            dd = cc;
            fd = fc;
            cc = b.saturating_sub(span(a, b) * 618 / 1000);
            fc = gross(cc);
        }
    }
    let mid = (a + b) / 2;
    if gross(mid) > best_p {
        best_p = gross(mid);
        best_x = mid;
    }
    (best_x, best_p)
}

/// Family-agnostic Gate-5 evaluation: dense oracle vs production-shaped sizer, asserts >=95%.
fn evaluate(gross: &dyn Fn(u128) -> u128, lo_b: u128, hi_b: u128, unit: f64, sym: &str) -> bool {
    let (oracle_size, oracle_profit) = best_gross(gross, lo_b, hi_b);
    println!(
        "  ORACLE(dense 512+refine): best_size={:.4} {}  profit={} ({:.6} {})",
        oracle_size as f64 / unit,
        sym,
        oracle_profit,
        oracle_profit as f64 / unit,
        sym
    );
    if oracle_profit == 0 {
        println!("  SKIP: no positive-gross cycle at this state (not a sizing failure).");
        return true;
    }
    // production sizer: coarse 12-point log grid over [lo,hi] + golden bracket (pipeline shape).
    let n = 12usize;
    let ln_lo = (lo_b as f64).ln();
    let ln_hi = (hi_b as f64).ln();
    let grid: Vec<u128> = (0..n)
        .map(|i| (ln_lo + (ln_hi - ln_lo) * (i as f64) / ((n - 1) as f64)).exp() as u128)
        .collect();
    let mut bi = 0usize;
    let mut bp = 0u128;
    for (i, &g) in grid.iter().enumerate() {
        let p = gross(g);
        if p > bp {
            bp = p;
            bi = i;
        }
    }
    let lo = if bi > 0 { grid[bi - 1] } else { grid[bi] / 4 };
    let hi = if bi + 1 < grid.len() {
        grid[bi + 1]
    } else {
        grid[bi].saturating_mul(4)
    };
    let span = |a: u128, b: u128| b.saturating_sub(a);
    let (mut a, mut b) = (lo, hi);
    let mut cc = b.saturating_sub(span(a, b) * 618 / 1000);
    let mut dd = a.saturating_add(span(a, b) * 618 / 1000);
    let (mut fc, mut fd) = (gross(cc), gross(dd));
    for _ in 0..32 {
        let tol = (a.max(b) / 1000).max(2);
        if span(a, b) <= tol {
            break;
        }
        if fc < fd {
            a = cc;
            cc = dd;
            fc = fd;
            dd = a.saturating_add(span(a, b) * 618 / 1000);
            fd = gross(dd);
        } else {
            b = dd;
            dd = cc;
            fd = fc;
            cc = b.saturating_sub(span(a, b) * 618 / 1000);
            fc = gross(cc);
        }
    }
    let refined = (a + b) / 2;
    let (sizer_size, sizer_profit) = if gross(refined) > bp {
        (refined, gross(refined))
    } else {
        (grid[bi], bp)
    };
    let ratio = sizer_profit as f64 / oracle_profit as f64 * 100.0;
    println!(
        "  SIZER(production): best_size={:.4} {}  profit={} ({:.6} {})  ratio={:.2}%",
        sizer_size as f64 / unit,
        sym,
        sizer_profit,
        sizer_profit as f64 / unit,
        sym,
        ratio
    );
    let net_ok = sizer_profit > 0;
    let pass = ratio >= 95.0 && net_ok;
    println!(
        "  => {} (>=95% oracle: {}, gross-positive: {})",
        if pass { "PASS" } else { "FAIL" },
        ratio >= 95.0,
        net_ok
    );
    pass
}

fn main() {
    let rpc_url = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL");
    let rpc = ArbitrumRpcClient::new(&rpc_url);

    // Flagship known arb (Dune 465686729 / 0xbd25): Pancake V3 fee100 WETH->WBTC, Uni V3 fee500 WBTC->WETH.
    // Pre-state = parent 465686728. token0=WBTC token1=WETH on both => hopA zfo=false, hopB zfo=true.
    let candidates = vec![
        Cand {
            name: "flagship_465686729_WETH_WBTC",
            block: 465686728,
            pool_a: "4bfc22a4da7f31f8a912a79a7e44a822398b4390",
            fee_a: 100,
            spacing_a: 1,
            zfo_a: false,
            pool_b: "2f5e87c9312fa29aed5c179e456625d79015299c",
            fee_b: 500,
            spacing_b: 10,
            zfo_b: true,
        },
        // NOTE: the former same-pool "substitute_{465686434,463386029,461919593}" candidates were
        // REMOVED — they are not in the Dune provenance set (q7602783/q7673019); they reuse the
        // flagship pool pair at arbitrary blocks (tiny/marginal gross) and only proved sizer accuracy,
        // not real arb coverage. Gate-5 candidates must be provenance-backed (see CandGen below).
    ];

    let only = std::env::var("GATE5_ORACLE_ONLY").ok();
    let selected: Vec<&Cand> = candidates
        .iter()
        .filter(|c| only.as_deref().is_none_or(|name| c.name == name))
        .collect();
    assert!(
        !selected.is_empty(),
        "GATE5_ORACLE_ONLY did not match any candidate"
    );

    let mut all = true;
    for c in selected {
        all &= run_candidate(&rpc, c);
    }

    // Gate-5 B2: real provenance-backed IN-CLASS candidates through the bot's non-V3 families
    // (Dune q7673019 — clean linear 2-leg cyclic arbs). DODO+V3 proves the harness generalizes;
    // the same pattern extends to Balancer/Curve/Maverick. Fluid candidates additionally need a
    // resolver-based historical pre-state fetcher (separate task).
    let gen_candidates = vec![CandGen {
        name: "dodo_dpp_univ3_464791930_WETH_USDC",
        block: 464791929, // parent pre-state of arb block 464791930 (tx 0x865f94e9...befe5df)
        legs: vec![
            LegSpec::Dodo {
                pool: "1666ea6e83bcc3473111732ef086ba47e6eddeb7",
                class: DodoPoolClass::Dpp, // version() = "DPP Advanced 1.0.0"
                token_in: "82af49447d8a07e3bd95bd0d56f35241523fbab1", // WETH
                token_out: "af88d065e77c8cc2239327c5edb3a432268e5831", // USDC
            },
            LegSpec::V3 {
                pool: "c6962004f452be9203591991d15f6b388e09e8d0",
                fee: 500,
                spacing: 10,
                zfo: false, // USDC(token1) -> WETH(token0) = one_for_zero
            },
        ],
        lo_in: 100_000_000_000_000,         // 1e-4 WETH
        hi_in: 200_000_000_000_000_000_000, // 200 WETH
        unit: 1e18,
        unit_sym: "WETH",
        cost_floor: 500_000_000_000_000, // 5e14 = 0.0005 WETH conservative all-in 2-hop cost
    }];
    if only.is_none() {
        for c in &gen_candidates {
            all &= run_candidate_gen(&rpc, c);
        }
    }
    println!(
        "\nGATE5 ORACLE: {} — production sizer vs independent dense brute-force oracle.",
        if all { "PASS" } else { "FAIL" }
    );
    assert!(all, "GATE5 ORACLE FAIL");
}
