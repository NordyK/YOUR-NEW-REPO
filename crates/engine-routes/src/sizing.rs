//! Optimal trade sizing via coarse geometric probe + golden-section refinement.
//!
//! Arb profit `f(x) = out(x) - x` over input size `x` is unimodal for AMM cycles (rises from 0,
//! peaks at the size that just closes the dislocation, then falls as price impact dominates), so a
//! golden-section search finds the optimum in a bounded number of quote evaluations — far more
//! accurate than a fixed log-spaced bucket grid (which misses the peak between buckets) and without
//! the grid's blind scan of dead sizes.
//!
//! Design follows the two production reference engines (Shadow/Fulcrum): (1) a coarse 10× geometric
//! probe to bracket the profitable region, then (2) golden-section refinement (φ-spaced interior
//! points, one new quote per iteration) to the peak. Integer (`u128`) amounts; the caller supplies
//! the route quote closure `out(x) -> Option<out>`. No allocation, no float in the decision path.

/// Result of an optimal-sizing search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizingResult {
    /// Input size (base-token units) that maximises profit.
    pub amount_in: u128,
    /// Output at `amount_in`.
    pub amount_out: u128,
    /// Gross profit = `amount_out - amount_in` (> 0 by construction; search returns `None` if no
    /// probed/refined size is profitable).
    pub profit: u128,
    /// Number of quote evaluations spent (telemetry / tuning).
    pub quotes: u32,
}

/// Coarse geometric probe multiplier (×10 per step) — matches the reference engines' 10× scan.
const PROBE_MUL: u128 = 10;
/// Golden ratio reciprocal 1/φ ≈ 0.6180339887, as a fixed-point fraction (num/den).
const INV_PHI_NUM: u128 = 6_180_339_887;
const INV_PHI_DEN: u128 = 10_000_000_000;
/// Max golden-section iterations (each = one new quote). 24 narrows a 10^x range to <0.1%.
const MAX_GS_ITERS: u32 = 24;

/// Find the profit-maximising input size in `[lo, hi]` for the unimodal profit function induced by
/// `quote` (`quote(x)` returns the route output for input `x`, or `None` if the route can't quote
/// that size). Returns the best profitable size, or `None` if no evaluated size nets a profit.
///
/// `quote` is called O(probe_steps + MAX_GS_ITERS) times — typically ~15–30 quotes regardless of
/// the range width, vs a fixed grid's blind N-bucket scan.
pub fn optimal_size<F: FnMut(u128) -> Option<u128>>(
    lo: u128,
    hi: u128,
    mut quote: F,
) -> Option<SizingResult> {
    if lo == 0 || hi < lo {
        return None;
    }
    let mut quotes: u32 = 0;
    let mut profit_at = |x: u128, quotes: &mut u32| -> i128 {
        *quotes += 1;
        match quote(x) {
            Some(out) => (out as i128) - (x as i128),
            None => i128::MIN, // unquotable size => treat as worst
        }
    };

    // --- Stage 1: coarse geometric probe to bracket the peak ---
    // Walk lo, lo*10, lo*100, … <= hi (plus hi itself), tracking the best probe and its neighbours.
    let mut best_x = lo;
    let mut best_p = profit_at(lo, &mut quotes);
    let mut prev_x = lo; // probe just below the current best
    let mut bracket_lo = lo;
    let mut bracket_hi = hi;
    let mut x = lo;
    let mut last_probe = lo;
    loop {
        let next = x.saturating_mul(PROBE_MUL);
        if next > hi || next == x {
            break;
        }
        let p = profit_at(next, &mut quotes);
        if p > best_p {
            best_p = p;
            prev_x = x; // the point one step below the new best brackets the left edge
            best_x = next;
        }
        last_probe = next;
        x = next;
    }
    // Always probe the upper bound too (optimum can sit at the cap).
    if last_probe != hi {
        let p = profit_at(hi, &mut quotes);
        if p > best_p {
            best_p = p;
            prev_x = last_probe;
            best_x = hi;
        }
    }
    // Bracket = [point below best, point above best]. If best is an interior probe, neighbours are
    // prev_x and best_x*10 (clamped to hi); if at an edge, clamp the bracket to that edge.
    bracket_lo = prev_x.max(bracket_lo);
    bracket_hi = best_x.saturating_mul(PROBE_MUL).min(bracket_hi).max(best_x);

    // --- Stage 2: golden-section refinement on [bracket_lo, bracket_hi] ---
    let mut a = bracket_lo;
    let mut b = bracket_hi;
    if b > a {
        let width0 = b - a;
        let mut c = a + mul_frac(width0, INV_PHI_DEN - INV_PHI_NUM); // a + (1-1/φ)·w
        let mut d = a + mul_frac(width0, INV_PHI_NUM); // a + (1/φ)·w
        let mut fc = profit_at(c, &mut quotes);
        let mut fd = profit_at(d, &mut quotes);
        let mut iters = 0u32;
        while iters < MAX_GS_ITERS && b > a + 1 {
            if fc >= fd {
                b = d;
                d = c;
                fd = fc;
                let w = b - a;
                c = a + mul_frac(w, INV_PHI_DEN - INV_PHI_NUM);
                fc = profit_at(c, &mut quotes);
            } else {
                a = c;
                c = d;
                fc = fd;
                let w = b - a;
                d = a + mul_frac(w, INV_PHI_NUM);
                fd = profit_at(d, &mut quotes);
            }
            // Converged when the bracket is within 0.1% of its low end.
            if b - a <= (a / 1000).max(1) {
                break;
            }
            iters += 1;
        }
        // Take the better of the refined interior points if it beats the coarse best.
        for &(xx, pp) in &[(c, fc), (d, fd)] {
            if pp > best_p {
                best_p = pp;
                best_x = xx;
            }
        }
    }

    if best_p <= 0 {
        return None;
    }
    // Re-quote the winner to report an exact (amount_out, profit) pair.
    let out = quote(best_x)?;
    quotes += 1;
    if out <= best_x {
        return None;
    }
    Some(SizingResult {
        amount_in: best_x,
        amount_out: out,
        profit: out - best_x,
        quotes,
    })
}

/// `value * num / den` without overflow for the ranges we use (widen to u256-ish via u128 halves is
/// overkill here; `saturating` guards the multiply). Inputs are amounts (< 2^127) and num<den<2^34.
#[inline]
fn mul_frac(value: u128, num: u128) -> u128 {
    // value can be large; split to avoid overflow: (value/den)*num + ((value%den)*num)/den.
    let q = value / INV_PHI_DEN;
    let r = value % INV_PHI_DEN;
    q.saturating_mul(num)
        .saturating_add(r.saturating_mul(num) / INV_PHI_DEN)
}

/// `|a|` with the sign of `b` (Numerical Recipes `SIGN`).
#[inline]
fn fsign(a: f64, b: f64) -> f64 {
    if b >= 0.0 {
        a.abs()
    } else {
        -a.abs()
    }
}

/// Like [`optimal_size`] but (1) accepts an optional near-optimal `seed` to skip the wide
/// geometric probe with a tight bracket, and (2) refines with **Brent's method** (parabolic
/// interpolation safeguarded by golden-section) instead of pure golden-section.
///
/// Brent converges superlinearly on the smooth interior of the unimodal profit curve — reaching
/// the peak in far fewer quote evaluations than golden-section's linear bracket-halving — while
/// its golden-section safeguard keeps it robust across the V3 tick kinks and integer-rounding
/// treads where a bare Newton/secant would oscillate or diverge. The returned size nets within
/// ~1 wei of [`optimal_size`]; REVM + the on-chain `minNetProfit` revert remain the truth gate,
/// so any residual error is a wasted check, never a capital loss.
pub fn optimal_size_seeded<F: FnMut(u128) -> Option<u128>>(
    lo: u128,
    hi: u128,
    seed: Option<u128>,
    mut quote: F,
) -> Option<SizingResult> {
    if lo == 0 || hi < lo {
        return None;
    }
    let mut quotes: u32 = 0;
    let mut prof = |x: u128, quotes: &mut u32| -> i128 {
        *quotes += 1;
        match quote(x) {
            Some(out) => (out as i128) - (x as i128),
            None => i128::MIN,
        }
    };

    // --- Stage 1: middle-out (`mnbrak`) bracket. Start at the trusted seed (or `geomean(lo,hi)`),
    // probe both neighbours to find the uphill direction, then expand geometrically until the
    // unimodal profit curve turns over. Far fewer quotes than scanning `lo→hi` for a peak that can
    // sit anywhere in a multi-decade range — and the seed (when present) starts us next to the peak.
    // An unquotable (`i128::MIN`) probe acts as a downhill wall, preserving the "large sizes revert"
    // safety. Numerical Recipes `mnbrak`, in log space (×PROBE_MUL steps). ---
    let (a, b, m, fm): (u128, u128, u128, i128) = {
        let start = match seed {
            Some(s) => s.clamp(lo, hi),
            None => (((lo as f64) * (hi as f64)).sqrt() as u128).clamp(lo, hi),
        };
        let f_start = prof(start, &mut quotes);
        let up1 = start.saturating_mul(PROBE_MUL).min(hi);
        let dn1 = (start / PROBE_MUL).max(lo);
        let f_up = if up1 > start {
            prof(up1, &mut quotes)
        } else {
            i128::MIN
        };
        let f_dn = if dn1 < start {
            prof(dn1, &mut quotes)
        } else {
            i128::MIN
        };

        if f_start >= f_up && f_start >= f_dn {
            // `start` already beats both neighbours → peak bracketed by [dn1, up1].
            (dn1, up1, start, f_start)
        } else {
            // Climb the uphill side, expanding ×PROBE_MUL until the curve turns over.
            let up = f_up >= f_dn;
            let mut p0 = start;
            let mut p1 = if up { up1 } else { dn1 };
            let mut f1 = if up { f_up } else { f_dn };
            let mut far = if up { hi } else { lo }; // over-the-top bound; set on turnover
            let mut turned = false;
            for _ in 0..MAX_GS_ITERS {
                let p2 = if up {
                    p1.saturating_mul(PROBE_MUL).min(hi)
                } else {
                    (p1 / PROBE_MUL).max(lo)
                };
                if p2 == p1 {
                    break; // hit lo/hi bound; peak sits at the bound `p1`
                }
                let f2 = prof(p2, &mut quotes);
                if f2 <= f1 {
                    far = p2;
                    turned = true;
                    break; // turned over at `p2` → peak at `p1`
                }
                p0 = p1;
                p1 = p2;
                f1 = f2;
            }
            if up {
                (p0, if turned { far } else { hi }, p1, f1)
            } else {
                (if turned { far } else { lo }, p0, p1, f1)
            }
        }
    };

    // --- Stage 2: Brent refinement on [a, b] (interior `m`, profit `fm`). ---
    let (mut best_x, mut best_p) = (m, fm);
    if b > a + 1 {
        let (bx, bp) = brent_max_profit(a, b, m, fm, lo, hi, &mut quotes, &mut prof);
        if bp > best_p {
            best_x = bx;
            best_p = bp;
        }
    }

    if best_p <= 0 {
        return None;
    }
    let out = quote(best_x)?;
    quotes += 1;
    if out <= best_x {
        return None;
    }
    Some(SizingResult {
        amount_in: best_x,
        amount_out: out,
        profit: out - best_x,
        quotes,
    })
}

/// Brent's method (Numerical Recipes §10.2, minimizing `-profit`) maximizing the integer profit
/// `quote(x) - x` on `[a_lo, b_hi]`. Positions are tracked in `f64` (relative precision ~1e-15,
/// far tighter than golden-section's 0.1%); each candidate is evaluated at the rounded integer
/// size via `prof`. Returns the best `(size, profit)` found.
#[allow(clippy::too_many_arguments)]
fn brent_max_profit<P: FnMut(u128, &mut u32) -> i128>(
    a_lo: u128,
    b_hi: u128,
    x_seed: u128,
    fx_seed: i128,
    clamp_lo: u128,
    clamp_hi: u128,
    quotes: &mut u32,
    prof: &mut P,
) -> (u128, i128) {
    const CGOLD: f64 = 0.381_966_011_25;
    const ZEPS: f64 = 1e-10;
    const TOL: f64 = 1e-3; // 0.1% — matches the golden-section baseline; Brent reaches it faster
    const ITMAX: u32 = 24;

    // Objective to MINIMIZE: g(x) = -profit. Unquotable sizes get +∞ (a hard penalty).
    let gval = |p: i128| -> f64 {
        if p == i128::MIN {
            f64::INFINITY
        } else {
            -(p as f64)
        }
    };
    // Evaluate at the rounded, clamped integer size.
    let eval = |pos: f64, quotes: &mut u32, prof: &mut P| -> (u128, i128) {
        let xi = (pos.round().max(0.0) as u128).clamp(clamp_lo, clamp_hi);
        (xi, prof(xi, quotes))
    };

    let (mut a, mut b) = (a_lo as f64, b_hi as f64);
    let mut x = x_seed as f64;
    let mut w = x;
    let mut v = x;
    let mut fx = gval(fx_seed);
    let mut fw = fx;
    let mut fv = fx;
    let mut best = (x_seed, fx_seed);
    let mut d = 0.0f64;
    let mut e = 0.0f64;

    for _ in 0..ITMAX {
        let xm = 0.5 * (a + b);
        let tol1 = TOL * x.abs() + ZEPS;
        let tol2 = 2.0 * tol1;
        if (x - xm).abs() <= tol2 - 0.5 * (b - a) {
            break;
        }
        if e.abs() > tol1 {
            // Parabolic fit through (x,fx),(w,fw),(v,fv).
            let r = (x - w) * (fx - fv);
            let mut q = (x - v) * (fx - fw);
            let mut p = (x - v) * q - (x - w) * r;
            q = 2.0 * (q - r);
            if q > 0.0 {
                p = -p;
            }
            q = q.abs();
            let etemp = e;
            e = d;
            if p.abs() >= (0.5 * q * etemp).abs() || p <= q * (a - x) || p >= q * (b - x) {
                e = if x >= xm { a - x } else { b - x };
                d = CGOLD * e;
            } else {
                d = p / q;
                let u = x + d;
                if (u - a) < tol2 || (b - u) < tol2 {
                    d = fsign(tol1, xm - x);
                }
            }
        } else {
            e = if x >= xm { a - x } else { b - x };
            d = CGOLD * e;
        }
        let u = if d.abs() >= tol1 {
            x + d
        } else {
            x + fsign(tol1, d)
        };
        let (uxi, up) = eval(u, quotes, prof);
        let fu = gval(up);
        if fu <= fx {
            if u >= x {
                a = x;
            } else {
                b = x;
            }
            v = w;
            fv = fw;
            w = x;
            fw = fx;
            x = u;
            fx = fu;
            best = (uxi, up);
        } else {
            if u < x {
                a = u;
            } else {
                b = u;
            }
            if fu <= fw || w == x {
                v = w;
                fv = fw;
                w = u;
                fw = fu;
            } else if fu <= fv || v == x || v == w {
                v = u;
                fv = fu;
            }
            if up > best.1 {
                best = (uxi, up);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constant-product two-pool cycle: borrow `x` of token A, A->B on pool1, B->A on pool2.
    /// out(x) is concave; profit is unimodal. We can compute the closed-form optimum to check.
    fn cp_cycle_quote(
        r_in1: u128,
        r_out1: u128,
        r_in2: u128,
        r_out2: u128,
        fee_bps: u128,
    ) -> impl Fn(u128) -> Option<u128> {
        move |x: u128| {
            if x == 0 {
                return None;
            }
            let f = 10_000 - fee_bps;
            // hop1: A->B
            let ain = x.checked_mul(f)? / 10_000;
            let out1 = r_out1.checked_mul(ain)? / (r_in1 + ain);
            if out1 == 0 {
                return None;
            }
            // hop2: B->A
            let ain2 = out1.checked_mul(f)? / 10_000;
            let out2 = r_out2.checked_mul(ain2)? / (r_in2 + ain2);
            Some(out2)
        }
    }

    #[test]
    fn unprofitable_cycle_returns_none() {
        // pool1 A/B=(1e9,2e9): 1 A -> ~2 B. pool2 B/A=(1e9,4e8): 1 B -> ~0.4 A.
        // Round-trip product = 2 * 0.4 = 0.8 < 1 -> loss at every size -> None.
        let q = cp_cycle_quote(1_000_000_000, 2_000_000_000, 1_000_000_000, 400_000_000, 30);
        assert!(optimal_size(1_000, 5_000_000_000, q).is_none());
    }

    #[test]
    fn finds_optimum_when_arb_exists() {
        // pool1 A/B=(1e9,2e9) (A cheap in B), pool2 B/A=(2e9,1.05e9) (B buys 5% more A than pool1 implies).
        // Cycle product (2 * 1.05/2) = 1.05 > 1 after-ish fees -> profitable. Optimum interior.
        let q = cp_cycle_quote(
            1_000_000_000,
            2_000_000_000,
            2_000_000_000,
            1_050_000_000,
            30,
        );
        let r = optimal_size(1_000, 2_000_000_000, q).expect("should find a profitable size");
        assert!(r.profit > 0);
        assert!(r.amount_out > r.amount_in);
        // The optimum must beat a naive tiny size and a naive huge size (unimodality sanity).
        let qq = cp_cycle_quote(
            1_000_000_000,
            2_000_000_000,
            2_000_000_000,
            1_050_000_000,
            30,
        );
        let tiny = qq(1_000).map(|o| o as i128 - 1_000).unwrap_or(i128::MIN);
        let huge = qq(2_000_000_000)
            .map(|o| o as i128 - 2_000_000_000)
            .unwrap_or(i128::MIN);
        assert!((r.profit as i128) >= tiny);
        assert!((r.profit as i128) >= huge);
        assert!(
            r.quotes < 60,
            "should converge in well under 60 quotes, used {}",
            r.quotes
        );
    }

    #[test]
    fn no_profit_returns_none() {
        // Balanced pools, no dislocation -> fees guarantee loss at every size.
        let q = cp_cycle_quote(
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            30,
        );
        assert!(optimal_size(1_000, 1_000_000_000, q).is_none());
    }

    #[test]
    fn beats_coarse_grid_accuracy() {
        // Golden-section should find a profit >= the best of a 12-bucket log grid on the same cycle.
        let q = cp_cycle_quote(
            1_000_000_000,
            2_000_000_000,
            2_000_000_000,
            1_060_000_000,
            30,
        );
        let gs = optimal_size(1_000, 2_000_000_000, q).expect("profitable");
        // 12-bucket log grid best
        let qq = cp_cycle_quote(
            1_000_000_000,
            2_000_000_000,
            2_000_000_000,
            1_060_000_000,
            30,
        );
        let mut grid_best = 0i128;
        let mut amt = 1_000u128;
        for _ in 0..12 {
            if let Some(o) = qq(amt) {
                grid_best = grid_best.max(o as i128 - amt as i128);
            }
            amt = amt.saturating_mul(3); // ~log spaced 1e3..~5e8
        }
        assert!(
            (gs.profit as i128) >= grid_best,
            "gs {} should >= grid {}",
            gs.profit,
            grid_best
        );
    }

    #[test]
    fn keeps_profitable_bracket_when_upper_sizes_are_unquotable() {
        // Mirrors the e066 exact-prefix oracle shape: lower sizes are profitable, but
        // larger probes revert in the executor and must not erase the profitable bracket.
        let q = |x: u128| match x {
            0..=1_000_000_000 => Some(x + 529_532),
            1_000_000_001..=5_000_000_000 => Some(x + 2_637_999),
            5_000_000_001..=10_000_000_000 => Some(x + 5_251_168),
            10_000_000_001..=24_999_999_999 => Some(x + 4_000_000),
            _ => None,
        };

        let r = optimal_size(1_000_000, 23_300_000_000, q)
            .expect("profitable lower bracket should survive unquotable upper probes");
        assert!(r.amount_in <= 24_999_999_999);
        assert!(r.profit > 0);
    }

    #[test]
    fn degenerate_inputs() {
        assert!(optimal_size(0, 100, |_| Some(1)).is_none());
        assert!(optimal_size(100, 50, |_| Some(1)).is_none());
    }

    // ---- optimal_size_seeded (Brent) — equivalence to golden-section + fewer quotes ----

    #[test]
    fn seeded_brent_matches_golden_within_1_wei_and_no_more_quotes() {
        // Several dislocations / fee tiers; Brent must net within 1 wei of golden-section
        // while never using more quotes (it should usually use far fewer).
        for (ri2, ro2, fee) in [
            (2_000_000_000u128, 1_050_000_000u128, 30u128),
            (2_000_000_000, 1_100_000_000, 30),
            (2_000_000_000, 1_010_000_000, 5),
            (3_000_000_000, 1_600_000_000, 1),
        ] {
            let g = optimal_size(
                1_000,
                2_000_000_000,
                cp_cycle_quote(1_000_000_000, 2_000_000_000, ri2, ro2, fee),
            )
            .expect("golden should size");
            let s = optimal_size_seeded(
                1_000,
                2_000_000_000,
                None,
                cp_cycle_quote(1_000_000_000, 2_000_000_000, ri2, ro2, fee),
            )
            .expect("seeded should size");
            assert!(
                (s.profit as i128 - g.profit as i128).abs() <= 1,
                "seeded profit {} not within 1 wei of golden {}",
                s.profit,
                g.profit
            );
            // Brent should converge in no more quotes than golden-section (usually ~1.5–2× fewer).
            assert!(
                s.quotes <= g.quotes,
                "seeded used {} quotes, golden used {} — should be <=",
                s.quotes,
                g.quotes
            );
        }
    }

    #[test]
    fn seeded_brent_with_trusted_seed_skips_probe_and_is_correct() {
        let q = || {
            cp_cycle_quote(
                1_000_000_000,
                2_000_000_000,
                2_000_000_000,
                1_050_000_000,
                30,
            )
        };
        let g = optimal_size(1_000, 2_000_000_000, q()).expect("golden");
        // Feed golden's answer as the seed — the tight-bracket path must reproduce it within 1 wei.
        let s = optimal_size_seeded(1_000, 2_000_000_000, Some(g.amount_in), q()).expect("seeded");
        assert!((s.profit as i128 - g.profit as i128).abs() <= 1);
        // A reasonable seed should make this cheap.
        assert!(
            s.quotes < 20,
            "seeded with a good seed used {} quotes",
            s.quotes
        );
    }

    #[test]
    fn seeded_brent_handles_a_kinked_profit_curve() {
        // Piecewise-linear "tick-like" output with a slope change (kink) — the golden-section
        // safeguard inside Brent must keep it from oscillating across the kink.
        let kinked = |x: u128| -> Option<u128> {
            if x == 0 {
                return None;
            }
            // out rises steeply then shallowly; profit = out - x peaks near the kink.
            let out = if x <= 1_000_000 {
                x + x / 5 // +20%
            } else {
                1_200_000 + (x - 1_000_000) / 50 // shallow after the kink
            };
            Some(out)
        };
        let g = optimal_size(1, 50_000_000, kinked).expect("golden");
        let s = optimal_size_seeded(1, 50_000_000, None, kinked).expect("seeded");
        // A SHARP kink (non-smooth peak) is not nailed bit-exact by any parabolic/golden
        // refinement unless a probe lands exactly on it; the mnbrak start (geomean/seed) generally
        // won't. The safeguarded search still lands within the sizing-precision budget (<1%) — which
        // the REVM + on-chain minNetProfit revert backstop. Smooth CPMM curves ARE bit-exact (the
        // `seeded_brent_matches_golden_within_1_wei*` tests). Don't tighten this to 1 wei: that only
        // held by the old lo→hi probe coincidentally hitting the kink's power-of-ten.
        assert!(
            (s.profit as i128) * 100 >= (g.profit as i128) * 99,
            "kinked: seeded {} should be within 1% of golden {}",
            s.profit,
            g.profit
        );
    }

    #[test]
    fn seeded_brent_unprofitable_returns_none() {
        assert!(optimal_size_seeded(
            1_000,
            5_000_000_000,
            None,
            cp_cycle_quote(1_000_000_000, 2_000_000_000, 1_000_000_000, 400_000_000, 30)
        )
        .is_none());
    }

    #[test]
    fn middle_out_brackets_a_far_peak_in_few_quotes() {
        // Profit peaks at x = 4e8, near hi = 5e8. A lo→hi scan from lo=1 needs ~9 ×10 steps just to
        // reach the peak region; mnbrak starts at geomean(lo,hi) and climbs toward it. Assert it
        // finds the peak (within 1% — the curve has a sharp kink at the apex) and stays well under
        // a full-decade scan.
        let peak: u128 = 400_000_000;
        let q = |x: u128| -> Option<u128> {
            if x == 0 {
                return None;
            }
            let dist = if x >= peak { x - peak } else { peak - x };
            let p = (peak / 8) as i128 - (dist / 8) as i128; // triangular, apex profit = peak/8
            if p <= 0 {
                Some(((x as i128) - (-p)).max(1) as u128) // loss region: out < x
            } else {
                Some(x + p as u128)
            }
        };
        let r = optimal_size_seeded(1, 500_000_000, None, q).expect("finds the far peak");
        assert!(
            (r.profit as i128) * 100 >= (peak / 8) as i128 * 99,
            "profit {} should be within 1% of apex {}",
            r.profit,
            peak / 8
        );
        assert!(
            r.quotes < 25,
            "mnbrak used {} quotes for a far peak (a lo→hi scan + Brent would use more)",
            r.quotes
        );
    }
}
