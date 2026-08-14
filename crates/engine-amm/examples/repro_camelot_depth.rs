// Offline reproduction: feed the V3 kernel the REAL on-chain Camelot ARB/USDC
// state measured from Dune at the run's block window, and quote a ~$83K swap.
// Decides kernel-vs-state for the inflation finding.
use alloy_primitives::U256;
use engine_amm::{v3_get_amount_out, TickInfo, V3Slot0};

fn main() {
    // Camelot ARB/USDC 0xfae2…fea1, from algebrapool_evt_swap near 2026-05-23 18:30 UTC.
    // token0 = ARB (18 dec), token1 = USDC (6 dec).
    let l: u128 = 289_196_844_807_085_867;
    let sqrt_price_x96 = U256::from_str_radix("26224753717060751589376", 10).unwrap();
    let tick: i32 = -298_438;

    // ~40 ETH worth of ARB ≈ $83,040 / $0.109 ≈ 762,000 ARB (raw 18-dec).
    let arb_in = U256::from(762_000u64) * U256::from(10u64).pow(U256::from(18u64));

    let show = |r: &Result<engine_amm::uniswap_v3::SwapOutput, engine_amm::AmmError>| match r {
        Ok(o) => {
            let usdc = o.amount_out.to::<u128>() as f64 / 1e6;
            format!(
                "out={} USDC (${:.0})  -> input was $83040",
                o.amount_out, usdc
            )
        }
        Err(e) => format!("Err({e:?})  (kernel refuses — fail-closed)"),
    };

    for (label, fee_pips, spacing) in [("fee=0.05%", 500u32, 60i32), ("fee=0.3%", 3000, 60)] {
        let slot0 = V3Slot0 {
            sqrt_price_x96,
            tick,
            fee_pips,
            liquidity: l,
            tick_spacing: spacing,
        };
        let empty: Vec<TickInfo> = vec![];
        let a = v3_get_amount_out(slot0, arb_in, true, &empty);
        let win: Vec<TickInfo> = vec![
            TickInfo {
                tick: tick - spacing * 16,
                liquidity_net: 0,
            },
            TickInfo {
                tick: tick + spacing * 16,
                liquidity_net: 0,
            },
        ];
        let b = v3_get_amount_out(slot0, arb_in, true, &win);
        println!("[{label}] in=762000 ARB (~$83040), ARB->USDC");
        println!("   empty-window : {}", show(&a));
        println!("   ±16t window  : {}", show(&b));
    }
}
