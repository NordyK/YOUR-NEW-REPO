use alloy_primitives::U256;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_amm::{sqrt_price_at_tick, v2_get_amount_out, v3_get_amount_out, TickInfo, V3Slot0};
use engine_routes::{optimal_size, optimal_size_seeded};
use engine_sim::{bounded_golden_section_v2v2, closed_form_v2v2, grid_search_v2v2};

fn bench_grid_search(c: &mut Criterion) {
    let grid = vec![1_000u128, 10_000, 100_000, 1_000_000, 10_000_000];
    c.bench_function("sim_grid_search_v2v2_5buckets", |b| {
        b.iter(|| {
            grid_search_v2v2(
                black_box(&grid),
                black_box(1_000_000_000u128),
                black_box(4_000_000_000u128),
                black_box(30),
                black_box(4_000_000_000u128),
                black_box(1_010_000_000u128),
                black_box(30),
            )
        })
    });
}

fn bench_golden_section(c: &mut Criterion) {
    c.bench_function("sim_golden_section_v2v2_32iter", |b| {
        b.iter(|| {
            bounded_golden_section_v2v2(
                black_box(1),
                black_box(10_000_000),
                black_box(1_000_000_000u128),
                black_box(4_000_000_000u128),
                black_box(30),
                black_box(4_000_000_000u128),
                black_box(1_010_000_000u128),
                black_box(30),
                black_box(32),
            )
        })
    });
}

fn bench_closed_form(c: &mut Criterion) {
    // Same V2/V2 cycle as `bench_golden_section`, so the two times are directly comparable.
    c.bench_function("sim_closed_form_v2v2", |b| {
        b.iter(|| {
            closed_form_v2v2(
                black_box(1),
                black_box(10_000_000),
                black_box(1_000_000_000u128),
                black_box(4_000_000_000u128),
                black_box(30),
                black_box(4_000_000_000u128),
                black_box(1_010_000_000u128),
                black_box(30),
            )
        })
    });
}

// ---- Non-V2 families: golden-section vs Brent on the SAME realistic quote ----
// These call `engine_routes::optimal_size` (golden) and `optimal_size_seeded` (Brent) over the
// identical quote closure, so the time ratio is the real sizing speedup for the V3/mixed/n-hop
// families that route through `size_cycle`/`optimize_cycle_amount`.

/// A profitable V3/V3 two-hop cycle priced by the real V3 kernel (no tick crossing — the common
/// in-range case, ~2.9 µs/quote). Pool 2 sits ~5 % higher in price, so the round trip nets ~5 %
/// before slippage and the optimum is interior.
fn bench_v3v3_golden_vs_brent(c: &mut Criterion) {
    let pool1 = V3Slot0 {
        sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
        tick: 0,
        fee_pips: 500,
        liquidity: 1_000_000_000_000_000_000_000u128, // 1e21
        tick_spacing: 10,
    };
    let pool2 = V3Slot0 {
        sqrt_price_x96: sqrt_price_at_tick(500).unwrap(), // price ~1.051
        tick: 500,
        fee_pips: 500,
        liquidity: 1_000_000_000_000_000_000_000u128,
        tick_spacing: 10,
    };
    let ticks = vec![
        TickInfo {
            tick: -20_000,
            liquidity_net: 0,
        },
        TickInfo {
            tick: 20_000,
            liquidity_net: 0,
        },
    ];
    const LO: u128 = 1_000_000_000; // 1e9
    const HI: u128 = 100_000_000_000_000_000_000; // 1e20
    let quote = |x: u128| -> Option<u128> {
        let mid = v3_get_amount_out(pool1, U256::from(x), true, &ticks).ok()?;
        let mid_u = u128::try_from(mid.amount_out).ok()?;
        if mid_u == 0 {
            return None;
        }
        let out = v3_get_amount_out(pool2, U256::from(mid_u), true, &ticks).ok()?;
        u128::try_from(out.amount_out).ok()
    };

    c.bench_function("sim_v3v3_golden_section", |b| {
        b.iter(|| optimal_size(black_box(LO), black_box(HI), &quote))
    });
    c.bench_function("sim_v3v3_brent", |b| {
        b.iter(|| optimal_size_seeded(black_box(LO), black_box(HI), None, &quote))
    });
}

/// A profitable 3-hop V2 triangle (A→B→C→A) priced by the real V2 kernel. Representative of the
/// `evaluate_triangle` path (3-hop cycles = ~86 % of the live candidate flow).
fn bench_triangle_golden_vs_brent(c: &mut Criterion) {
    // Reserves chosen so the loop nets a few % before slippage.
    let legs = [
        (1_000_000_000_000u128, 2_000_000_000_000u128, 30u32),
        (2_000_000_000_000u128, 3_000_000_000_000u128, 30u32),
        (3_000_000_000_000u128, 1_050_000_000_000u128, 30u32),
    ];
    const LO: u128 = 1_000_000;
    const HI: u128 = 500_000_000_000;
    let quote = |x: u128| -> Option<u128> {
        let mut amt = x;
        for (r_in, r_out, fee) in legs {
            amt = v2_get_amount_out(amt, r_in, r_out, fee).ok()?;
            if amt == 0 {
                return None;
            }
        }
        Some(amt)
    };
    c.bench_function("sim_triangle3_golden_section", |b| {
        b.iter(|| optimal_size(black_box(LO), black_box(HI), &quote))
    });
    c.bench_function("sim_triangle3_brent", |b| {
        b.iter(|| optimal_size_seeded(black_box(LO), black_box(HI), None, &quote))
    });
}

/// A profitable mixed V2→V3 two-hop cycle (one constant-product leg, one concentrated-liquidity
/// leg). Representative of the `evaluate_two_hop_mixed` path.
fn bench_mixed_v2v3_golden_vs_brent(c: &mut Criterion) {
    let (r_in, r_out, fee) = (1_000_000_000_000u128, 2_000_000_000_000u128, 30u32); // V2: ~2 B per A
    let v3 = V3Slot0 {
        sqrt_price_x96: sqrt_price_at_tick(-6_000).unwrap(), // price(A/B) ~0.549 → round trip ~+9%
        tick: -6_000,
        fee_pips: 500,
        liquidity: 1_000_000_000_000_000_000_000u128,
        tick_spacing: 10,
    };
    let ticks = vec![
        TickInfo {
            tick: -26_000,
            liquidity_net: 0,
        },
        TickInfo {
            tick: 14_000,
            liquidity_net: 0,
        },
    ];
    const LO: u128 = 1_000_000;
    const HI: u128 = 200_000_000_000;
    let quote = |x: u128| -> Option<u128> {
        let mid = v2_get_amount_out(x, r_in, r_out, fee).ok()?;
        if mid == 0 {
            return None;
        }
        let out = v3_get_amount_out(v3, U256::from(mid), true, &ticks).ok()?;
        u128::try_from(out.amount_out).ok()
    };
    c.bench_function("sim_mixed_v2v3_golden_section", |b| {
        b.iter(|| optimal_size(black_box(LO), black_box(HI), &quote))
    });
    c.bench_function("sim_mixed_v2v3_brent", |b| {
        b.iter(|| optimal_size_seeded(black_box(LO), black_box(HI), None, &quote))
    });
}

/// A profitable 4-hop V2 cycle — the deepest shape the executor sizes (`evaluate_n_hop_v3`,
/// MAX_HOPS path). Each extra hop multiplies the per-quote cost, so quote-count savings compound.
fn bench_fourhop_golden_vs_brent(c: &mut Criterion) {
    let legs = [
        (1_000_000_000_000u128, 2_000_000_000_000u128, 30u32),
        (2_000_000_000_000u128, 3_000_000_000_000u128, 30u32),
        (3_000_000_000_000u128, 4_000_000_000_000u128, 30u32),
        (4_000_000_000_000u128, 1_100_000_000_000u128, 30u32),
    ];
    const LO: u128 = 1_000_000;
    const HI: u128 = 500_000_000_000;
    let quote = |x: u128| -> Option<u128> {
        let mut amt = x;
        for (r_in, r_out, fee) in legs {
            amt = v2_get_amount_out(amt, r_in, r_out, fee).ok()?;
            if amt == 0 {
                return None;
            }
        }
        Some(amt)
    };
    c.bench_function("sim_fourhop_golden_section", |b| {
        b.iter(|| optimal_size(black_box(LO), black_box(HI), &quote))
    });
    c.bench_function("sim_fourhop_brent", |b| {
        b.iter(|| optimal_size_seeded(black_box(LO), black_box(HI), None, &quote))
    });
}

criterion_group!(
    benches,
    bench_grid_search,
    bench_golden_section,
    bench_closed_form,
    bench_v3v3_golden_vs_brent,
    bench_triangle_golden_vs_brent,
    bench_mixed_v2v3_golden_vs_brent,
    bench_fourhop_golden_vs_brent
);
criterion_main!(benches);
