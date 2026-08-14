//! Per-pool spot-rate cache (refresh-on-use, atomic-slot shared).
//!
//! `cycle_marginal_product` (the per-event route-ranking proxy) used to chase pool
//! meta + state and recompute every hop's spot rate on *every* dirty event — ~640
//! rate computes per event, even though a pool's rate only changes when that pool
//! itself swaps. This cache flips it to compute-once-per-move: on each dirty event the
//! sim thread recomputes the two directional rates for ONLY the pools that just moved
//! (`ev.pools`, ~1/event via `refresh_pool_spot`) and reads one `Relaxed` atomic load
//! per hop for every other hop in the cycle.
//!
//! ## Why the sim refreshes (not a "state-writer precompute")
//! [latency #7] The refresh is deliberately on the SIM thread at the point of use, not on
//! the state-writer, because pool state is committed by FIVE different dirty sources on
//! different cores — the feed forward-apply/state-writer, plus the periodic RPC resync, the
//! WSS log updater, the Fluid poller, and shadow-REVM (all core 0). Refreshing on-use covers
//! every source with one code path; a writer-only precompute would leave the spot stale for
//! the four non-feed sources → mis-ranked cycles. The rates are stored in `AtomicU64` slots so
//! the cache is shared-safe regardless. The residual cost (~1 spot compute/event on the sim
//! critical path) is negligible vs the ~640/event the cache already eliminated — moving it
//! would trade correctness (coverage) for a sub-µs saving, so it stays here by design.
//!
//! Determinism contract: the cached value is bit-identical to what the old inline
//! formula produced for that pool+direction. The ranking is only a *proxy* (it drives
//! cull + top-K order); exact accept/reject is still the integer quote-walk + REVM /
//! on-chain `minNetProfit`. So the cache preserves the three sentinels exactly:
//!   * a finite rate  → fold into the product (live V2/V3 hop),
//!   * `0.0`          → dead pool, cull the route (V2 zero-reserve / V3 zero-liquidity),
//!   * `INFINITY`     → exotic family with no cheap closed-form spot rate (never cull;
//!                      the exact dedicated evaluator decides).
//!
//! Fail-safe default: an entry that has never been written reads as `INFINITY`, i.e.
//! "never cull" — an un-prewarmed pool is evaluated rather than wrongly dropped.

use alloy_primitives::U256;
use engine_state::PoolId;
use std::sync::atomic::{AtomicU64, Ordering};

/// Convert a `U256` to `f64` by Horner-folding its four 64-bit limbs (little-endian)
/// at base 2^64. This is the canonical converter the V3 spot formula uses; the loss
/// of precision past 53 mantissa bits is intentional and matches the on-chain ranking
/// proxy exactly (the exact accept/reject is the integer kernel, not this f64).
#[inline]
pub(crate) fn u256_to_f64(x: U256) -> f64 {
    let l = x.as_limbs();
    const P64: f64 = 18_446_744_073_709_551_616.0; // 2^64
    ((l[3] as f64 * P64 + l[2] as f64) * P64 + l[1] as f64) * P64 + l[0] as f64
}

/// V2-family (constant-product) directional spot rate after fee, or `0.0` if the pool
/// is dead (either reserve zero). `dir0` = swap token0→token1, so the *input* reserve
/// is reserve0 and the rate is `reserve1/reserve0 * (1 - fee)`. `fee_bps` is in bps.
#[inline]
pub(crate) fn v2_spot_after_fee(reserve0: u128, reserve1: u128, fee_bps: u32, dir0: bool) -> f64 {
    let (r_in, r_out) = if dir0 {
        (reserve0 as f64, reserve1 as f64)
    } else {
        (reserve1 as f64, reserve0 as f64)
    };
    if r_in <= 0.0 || r_out <= 0.0 {
        return 0.0;
    }
    let fee_frac = (fee_bps as f64) / 10_000.0;
    (r_out / r_in) * (1.0 - fee_frac)
}

/// V3-family (sqrt-price) directional spot rate after fee, or `0.0` if dead (zero
/// liquidity, or a sqrt-price that squares to a non-finite / non-positive price).
/// `price = (sqrt_price_x96 / 2^96)^2` is token1-per-token0; `dir0` keeps it, the
/// reverse direction takes its reciprocal. `fee_ppm` is in parts-per-million.
#[inline]
pub(crate) fn v3_spot_after_fee(
    sqrt_price_x96: U256,
    liquidity: u128,
    fee_ppm: u32,
    dir0: bool,
) -> f64 {
    const Q96: f64 = 79_228_162_514_264_337_593_543_950_336.0; // 2^96
    if liquidity == 0 {
        return 0.0;
    }
    let sp = u256_to_f64(sqrt_price_x96) / Q96;
    let price = sp * sp; // token1 per token0
    if !(price.is_finite() && price > 0.0) {
        return 0.0;
    }
    let rate = if dir0 { price } else { 1.0 / price };
    let fee_frac = (fee_ppm as f64) / 1_000_000.0;
    rate * (1.0 - fee_frac)
}

/// Per-pool directional spot-rate cache. Two `f64` entries per pool (one per swap
/// direction) stored as raw bits in `AtomicU64` — a single machine word, so loads
/// and stores are torn-free without a lock. One writer (the state-writer thread)
/// per pool; many lock-free `Relaxed` readers (the sim shards).
pub struct PoolSpotCache {
    /// `[pool*2 + (dir0 ? 0 : 1)]` → cached rate bits. Length `2 * pool_count`.
    rates: Box<[AtomicU64]>,
}

impl PoolSpotCache {
    /// Build a cache for `pool_count` pools. Every entry starts at `INFINITY`
    /// (the fail-safe "never cull" sentinel) until the writer prewarms it.
    pub fn with_pool_count(pool_count: usize) -> Self {
        let rates = (0..pool_count.saturating_mul(2))
            .map(|_| AtomicU64::new(f64::INFINITY.to_bits()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { rates }
    }

    /// Read the cached spot rate for `pool` in the given direction (`dir0` = swap
    /// token0→token1). An out-of-range pool id reads as `INFINITY` (fail-safe).
    #[inline]
    pub fn get(&self, pool: PoolId, dir0: bool) -> f64 {
        let i = (pool.0 as usize) * 2 + usize::from(!dir0);
        match self.rates.get(i) {
            Some(slot) => f64::from_bits(slot.load(Ordering::Relaxed)),
            None => f64::INFINITY,
        }
    }

    /// Publish both directional rates for `pool` (computed by the writer on a state
    /// change). Out-of-range ids are silently ignored (the cache is sized once to
    /// the readiness-time pool count, matching the telemetry sizing discipline).
    #[inline]
    pub fn set(&self, pool: PoolId, dir0_rate: f64, dir1_rate: f64) {
        let base = (pool.0 as usize) * 2;
        if let Some(slot) = self.rates.get(base) {
            slot.store(dir0_rate.to_bits(), Ordering::Relaxed);
        }
        if let Some(slot) = self.rates.get(base + 1) {
            slot.store(dir1_rate.to_bits(), Ordering::Relaxed);
        }
    }

    /// Number of pools the cache is sized for.
    #[inline]
    pub fn pool_count(&self) -> usize {
        self.rates.len() / 2
    }

    /// Software-prefetch a pool's two directional rate words into L1. Both entries are
    /// adjacent (`base`, `base+1` = 16 bytes), so one hint warms the line the fold will
    /// read. A non-faulting hint with no observable effect; out-of-range is a no-op.
    #[inline]
    pub fn prefetch(&self, pool: PoolId) {
        let base = (pool.0 as usize) * 2;
        if let Some(slot) = self.rates.get(base) {
            engine_sysopt::prefetch::prefetch_read_t0(slot as *const AtomicU64);
        }
    }
}

/// Compute both directional spot rates for `pool` from current `universe` state, with
/// the *exact* family dispatch of the old inline `cycle_marginal_product`:
///   * V2-family (`UniswapV2`/`SushiV2`/`CamelotV2`) → constant-product rate,
///   * V3-family (`UniswapV3`/`SushiV3`/`PancakeV3`/`AlgebraV19`/`UniswapV4`) → sqrt-price rate,
///   * everything else → `INFINITY` (never cull).
///
/// Note the deliberate exclusions: `SolidlyV3` is V2-reserves-backed and
/// `KyberSwapElastic` is V3-state-backed, yet both fall through to `INFINITY` because
/// their curves aren't cheaply/correctly priced by the constant-product or sqrt-price
/// proxy — matching the original `_ =>` arm. Returns `[dir0_rate, dir1_rate]`.
pub fn compute_pool_spot(universe: &engine_state::Universe, pool: PoolId) -> [f64; 2] {
    use engine_state::PoolKind;
    let meta = universe.pools.meta(pool);
    let fee = meta.fee_pips_or_bps;
    match meta.kind {
        PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => {
            let s = universe.v2_state.snapshot(meta.state_slot);
            let (r0, r1) = (s.reserves.reserve0, s.reserves.reserve1);
            [
                v2_spot_after_fee(r0, r1, fee, true),
                v2_spot_after_fee(r0, r1, fee, false),
            ]
        }
        PoolKind::UniswapV3
        | PoolKind::SushiV3
        | PoolKind::PancakeV3
        | PoolKind::AlgebraV19
        | PoolKind::UniswapV4 => {
            let s = universe.v3_state.load(meta.state_slot);
            [
                v3_spot_after_fee(s.sqrt_price_x96, s.liquidity, fee, true),
                v3_spot_after_fee(s.sqrt_price_x96, s.liquidity, fee, false),
            ]
        }
        // [arb-mix FIX #4] Curve StableSwap-NG: cheap marginal spot so stablecoin arbs (USDC/USDT/
        // crvUSD — Curve is THE stable venue, 68% of real Arbitrum arbs involve stables) rank by their
        // REALIZABLE edge instead of the INFINITY-sentinel that Fix #3 de-prioritises. Only 2-coin
        // pools (the cache holds two directional rates); larger pools fall through to INFINITY.
        PoolKind::CurveStableSwapNg => curve_stable_spot(&universe.curve_state.snapshot(meta.state_slot)),
        _ => [f64::INFINITY, f64::INFINITY],
    }
}

/// Cheap after-fee marginal spot rate for a 2-coin Curve StableSwap-NG pool: `get_dy(dx)/dx` at a
/// small probe `dx` (≈1 bp of the balance), both directions, reusing the cached `D` (no Newton on
/// balanced pools). Returned in the same raw `out/in` convention as `v2_spot_after_fee` — decimals
/// cancel around a closed cycle. `n_coins != 2` or any error → `INFINITY` (never-cull fallback).
fn curve_stable_spot(slot: &engine_state::CurveSlot) -> [f64; 2] {
    let st = &slot.kernel;
    if st.n_coins != 2 {
        return [f64::INFINITY, f64::INFINITY];
    }
    let spot = |i: usize, j: usize| -> f64 {
        let bal = st.balances[i];
        if bal == 0 {
            return f64::INFINITY;
        }
        // 1 bp of the input balance: small enough to approximate the marginal, large enough to avoid
        // integer-rounding `dy == 0`. get_dy applies the pool fee, matching the after-fee convention.
        let dx = (bal / 10_000).max(1);
        match engine_amm::curve::get_dy_cached(st, i, j, dx, slot.d_cached) {
            Ok(dy) if dy > 0 => dy as f64 / dx as f64,
            _ => f64::INFINITY,
        }
    };
    [spot(0, 1), spot(1, 0)]
}

/// Recompute and publish `pool`'s two directional spot rates from current universe
/// state. Called by the state-writer thread immediately after it applies a pool's
/// state change (the precompute-on-write half of the cache), and once per pool at
/// boot to prewarm. Cheap: one `compute_pool_spot` + two atomic stores.
#[inline]
pub fn refresh_pool_spot(universe: &engine_state::Universe, cache: &PoolSpotCache, pool: PoolId) {
    let [d0, d1] = compute_pool_spot(universe, pool);
    cache.set(pool, d0, d1);
}

/// Build a cache sized to `universe.pools.len()` and prewarm every pool's rate. Run
/// once at startup so the first dirty event reads warm rates rather than the
/// fail-safe `INFINITY` default.
pub fn build_prewarmed_cache(universe: &engine_state::Universe) -> PoolSpotCache {
    let n = universe.pools.len();
    let cache = PoolSpotCache::with_pool_count(n);
    for i in 0..n {
        refresh_pool_spot(universe, &cache, PoolId(i as u32));
    }
    cache
}

/// Partial-sort `scored` so its top-`k` entries (highest score first) occupy `[0..k)`
/// in descending order, leaving the rest unspecified. When more than `k` routes survive
/// the cull, this is `select_nth_unstable` (O(n) partition) + a sort of just the `k`
/// survivors, instead of a full O(n log n) sort of all ≤256 — same top-`k` prefix, so
/// the lane1/lane2 split below is unchanged. NaN scores compare as `Equal` (sink), as
/// in the old comparator. `n <= k` degrades to a full sort.
#[inline]
pub(crate) fn partial_sort_desc_topk(scored: &mut [(engine_state::RouteId, f64)], k: usize) {
    let desc = |a: &(engine_state::RouteId, f64), b: &(engine_state::RouteId, f64)| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    };
    if scored.len() <= k {
        scored.sort_unstable_by(desc);
        return;
    }
    // Partition the k highest scores into [0..k) (unordered), then order just those.
    scored.select_nth_unstable_by(k - 1, desc);
    scored[..k].sort_unstable_by(desc);
}

/// Hot-path route-ranking core: fold the per-hop cached spot rates into the cycle's
/// marginal-product score. One `Relaxed` atomic load per hop (vs the old per-event
/// meta-chase + state-load + recompute). `dir0` = even direction bit, exactly as the
/// old inline code (`(hop.direction & 1) == 0`). Caller passes the live hop slice
/// (`&route.hops[..route.hop_count]`).
#[inline]
pub fn cycle_marginal_product_hops(cache: &PoolSpotCache, hops: &[engine_routes::Hop]) -> f64 {
    fold_marginal_product(
        hops.iter()
            .map(|hop| cache.get(hop.pool, (hop.direction & 1) == 0)),
    )
}

/// Fold per-hop cached spot rates into the route's marginal-product score, with the
/// *exact* short-circuit semantics of the old inline `cycle_marginal_product`:
///   * the first `0.0` (dead pool) returns `0.0` (cull),
///   * the first `INFINITY` (exotic family) returns `INFINITY` (never cull),
///   * otherwise the finite product, with no hops folding to the `1.0` identity.
///
/// V2/V3 rates are always finite and positive, so `INFINITY` is *only* ever the
/// exotic sentinel and `0.0` is *only* ever the dead sentinel — which is what makes
/// reading from a per-pool cache bit-identical to recomputing inline.
#[inline]
pub fn fold_marginal_product<I: IntoIterator<Item = f64>>(rates: I) -> f64 {
    let mut product = 1.0f64;
    for rate in rates {
        if rate == 0.0 {
            return 0.0;
        }
        if rate == f64::INFINITY {
            return f64::INFINITY;
        }
        product *= rate;
    }
    product
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use engine_routes::Hop;

    /// [arb-mix FIX #4] Curve stable spot: a balanced 2-coin pool yields a symmetric after-fee rate
    /// just below par (fee → no self-arb); imbalance breaks the symmetry (the scarce→abundant leg
    /// out-rates the reverse) so a cycle product can see the edge; non-2-coin → INFINITY fallback.
    #[test]
    fn curve_stable_spot_marginal_rate_reflects_balance_and_imbalance() {
        use engine_amm::curve::StableSwapState;
        use engine_state::CurveSlot;
        const E24: u128 = 1_000_000_000_000_000_000_000_000; // 1e24 raw units (deep pool)
        // A=100 (a_param = A·A_PRECISION), fee = 4 bps (4e6 in 1e10 units), 18-dp coins (rate 1e18 →
        // xp == balance), no dynamic offpeg component.
        let mk = |b0: u128, b1: u128| StableSwapState {
            n_coins: 2,
            balances: [b0, b1, 0, 0, 0, 0, 0, 0],
            rates: [1_000_000_000_000_000_000u128, 1_000_000_000_000_000_000, 0, 0, 0, 0, 0, 0],
            a_param: 10_000,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        let slot = |st: StableSwapState| CurveSlot {
            kernel: st,
            tokens: [[0u8; 20]; 8],
            d_cached: U256::ZERO,
        };

        // Balanced: both legs finite, symmetric, in (0.99, 1.0) — the fee keeps them below par.
        let bal = curve_stable_spot(&slot(mk(E24, E24)));
        assert!(bal[0].is_finite() && bal[1].is_finite());
        assert!((bal[0] - bal[1]).abs() < 1e-9, "balanced pool is symmetric: {bal:?}");
        assert!(bal[0] > 0.99 && bal[0] < 1.0, "after-fee balanced rate just below par: {}", bal[0]);

        // Imbalanced (coin 0 abundant, coin 1 scarce): 1->0 (scarce in) out-rates 0->1.
        let imb = curve_stable_spot(&slot(mk(2 * E24, E24)));
        assert!(imb[0].is_finite() && imb[1].is_finite());
        assert!(imb[1] > imb[0], "scarce->abundant leg out-rates the reverse: {imb:?}");

        // n_coins != 2 → INFINITY fallback (never-cull path, exotic-neutral under Fix #3).
        let mut three = mk(E24, E24);
        three.n_coins = 3;
        assert_eq!(curve_stable_spot(&slot(three)), [f64::INFINITY, f64::INFINITY]);
    }

    /// 2^96 — the V3 sqrt-price fixed-point scale.
    fn q96() -> U256 {
        U256::from(1u8) << 96
    }

    #[test]
    fn u256_to_f64_matches_limb_formula() {
        assert_eq!(u256_to_f64(U256::ZERO), 0.0);
        assert_eq!(u256_to_f64(U256::from(1u8)), 1.0);
        // 2^96 round-trips to the Q96 constant used by the V3 spot formula.
        const Q96: f64 = 79_228_162_514_264_337_593_543_950_336.0;
        assert_eq!(u256_to_f64(q96()), Q96);
        // 2^64 → exactly 2^64 in f64 (representable).
        assert_eq!(
            u256_to_f64(U256::from(1u128) << 64),
            18_446_744_073_709_551_616.0
        );
    }

    #[test]
    fn v2_spot_after_fee_matches_reference_and_culls_dead() {
        // dir0: (reserve1/reserve0) * (1 - fee_bps/1e4).
        assert_eq!(
            v2_spot_after_fee(1000, 2000, 30, true),
            (2000.0 / 1000.0) * (1.0 - 30.0 / 10_000.0)
        );
        // dir1: (reserve0/reserve1) * (1 - fee).
        assert_eq!(
            v2_spot_after_fee(1000, 2000, 30, false),
            (1000.0 / 2000.0) * (1.0 - 30.0 / 10_000.0)
        );
        // Either reserve at zero → dead → 0.0 (cull), both directions.
        assert_eq!(v2_spot_after_fee(0, 2000, 30, true), 0.0);
        assert_eq!(v2_spot_after_fee(1000, 0, 30, false), 0.0);
    }

    #[test]
    fn v3_spot_after_fee_matches_reference_and_culls_dead() {
        // sqrt = 2*Q96 → sp=2.0 → price=4.0. fee_ppm=0 keeps the check exact.
        let sqrt2 = q96() << 1;
        assert_eq!(v3_spot_after_fee(sqrt2, 1_000_000, 0, true), 4.0); // dir0 = price
        assert_eq!(v3_spot_after_fee(sqrt2, 1_000_000, 0, false), 0.25); // dir1 = 1/price
                                                                         // fee applies as ppm.
        let want = 4.0 * (1.0 - 3000.0 / 1_000_000.0);
        assert_eq!(v3_spot_after_fee(sqrt2, 1_000_000, 3000, true), want);
        // Zero liquidity → dead → 0.0 (cull).
        assert_eq!(v3_spot_after_fee(sqrt2, 0, 0, true), 0.0);
        // Zero sqrt-price → price not > 0 → dead → 0.0.
        assert_eq!(v3_spot_after_fee(U256::ZERO, 1_000_000, 0, true), 0.0);
    }

    /// Pools spanning every dispatch class, for the equivalence + dispatch tests.
    struct FamilyPools {
        v2: PoolId,
        v3: PoolId,
        solidly: PoolId,
        kyber: PoolId,
        dead: PoolId,
        sqrt2: U256,
    }

    fn build_family_universe() -> (
        std::sync::Arc<engine_state::Universe>,
        FamilyPools,
        engine_state::publication::StateWriterPermit,
    ) {
        use engine_state::{
            PoolKind, RiskClass, UniverseBuilder, V2Reserves256, V2State, V3State, VenueId,
        };
        let v2state = |r0: u128, r1: u128| V2State {
            reserves: V2Reserves256 {
                reserve0: r0,
                reserve1: r1,
                block_timestamp_last: 0,
            },
        };
        let v3state = |sqrt: U256, liq: u128, fee: u32| V3State {
            sqrt_price_x96: sqrt,
            tick: 0,
            liquidity: liq,
            fee_pips: fee,
            tick_spacing: 1,
            initialised_ticks: vec![],
        };
        let mut ub = UniverseBuilder::new();
        let a = ub.add_token([0xaa; 20], 18, RiskClass::Permissionless);
        let b = ub.add_token([0xbb; 20], 18, RiskClass::Permissionless);
        let c = ub.add_token([0xcc; 20], 18, RiskClass::Permissionless);
        let sqrt2 = (U256::from(1u8) << 96) << 1; // 2*Q96 → price = 4.0
        let v2 = ub
            .add_v2_pool(
                [0x01; 20],
                PoolKind::UniswapV2,
                a,
                b,
                VenueId(0),
                30,
                v2state(1000, 2000),
            )
            .unwrap();
        let v3 = ub
            .add_v3_pool(
                [0x02; 20],
                PoolKind::UniswapV3,
                b,
                c,
                VenueId(0),
                3000,
                1,
                v3state(sqrt2, 1_000_000, 3000),
            )
            .unwrap();
        // SolidlyV3 is V2-reserves-backed but its stable curve makes the constant-product
        // spot rate wrong → it must rank as INFINITY (never cull), NOT a V2 rate.
        let solidly = ub
            .add_v2_pool(
                [0x03; 20],
                PoolKind::SolidlyV3,
                a,
                c,
                VenueId(0),
                5,
                v2state(1000, 1000),
            )
            .unwrap();
        // KyberSwapElastic is V3-state-backed but excluded from the cheap-rank set → INFINITY.
        let kyber = ub
            .add_v3_pool(
                [0x04; 20],
                PoolKind::KyberSwapElastic,
                a,
                c,
                VenueId(0),
                1000,
                1,
                v3state(sqrt2, 1_000_000, 1000),
            )
            .unwrap();
        // A dead V2 pool (zero reserve) → 0.0 both directions (cull).
        let dead = ub
            .add_v2_pool(
                [0x05; 20],
                PoolKind::UniswapV2,
                b,
                c,
                VenueId(0),
                30,
                v2state(0, 2000),
            )
            .unwrap();
        // WI-06 F1: seal so the caller gets THIS universe's own sole-writer permit — the
        // legitimate witness for its resident stores (no forged/foreign permit).
        let (universe, permit) = ub.seal().into_parts();
        (
            universe,
            FamilyPools {
                v2,
                v3,
                solidly,
                kyber,
                dead,
                sqrt2,
            },
            permit,
        )
    }

    /// Reference oracle: the EXACT old inline `cycle_marginal_product` body (the spec
    /// the cache must reproduce bit-for-bit). Kept test-only; production reads the cache.
    fn reference_marginal_product(universe: &engine_state::Universe, hops: &[Hop]) -> f64 {
        use engine_state::PoolKind;
        const Q96: f64 = 79_228_162_514_264_337_593_543_950_336.0;
        let mut product = 1.0f64;
        for hop in hops {
            let meta = universe.pools.meta(hop.pool);
            let dir0 = (hop.direction & 1) == 0;
            let rate = match meta.kind {
                PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => {
                    let s = universe.v2_state.snapshot(meta.state_slot);
                    let (r_in, r_out) = if dir0 {
                        (s.reserves.reserve0 as f64, s.reserves.reserve1 as f64)
                    } else {
                        (s.reserves.reserve1 as f64, s.reserves.reserve0 as f64)
                    };
                    if r_in <= 0.0 || r_out <= 0.0 {
                        return 0.0;
                    }
                    let fee_frac = (meta.fee_pips_or_bps as f64) / 10_000.0;
                    (r_out / r_in) * (1.0 - fee_frac)
                }
                PoolKind::UniswapV3
                | PoolKind::SushiV3
                | PoolKind::PancakeV3
                | PoolKind::AlgebraV19
                | PoolKind::UniswapV4 => {
                    let s = universe.v3_state.load(meta.state_slot);
                    if s.liquidity == 0 {
                        return 0.0;
                    }
                    let sp = u256_to_f64(s.sqrt_price_x96) / Q96;
                    let price = sp * sp;
                    if !(price.is_finite() && price > 0.0) {
                        return 0.0;
                    }
                    let rate = if dir0 { price } else { 1.0 / price };
                    let fee_frac = (meta.fee_pips_or_bps as f64) / 1_000_000.0;
                    rate * (1.0 - fee_frac)
                }
                PoolKind::TraderJoeLb => return f64::INFINITY,
                _ => return f64::INFINITY,
            };
            product *= rate;
        }
        product
    }

    fn prewarm(universe: &engine_state::Universe) -> PoolSpotCache {
        let cache = PoolSpotCache::with_pool_count(universe.pools.len());
        for i in 0..universe.pools.len() {
            let p = PoolId(i as u32);
            let [d0, d1] = compute_pool_spot(universe, p);
            cache.set(p, d0, d1);
        }
        cache
    }

    #[test]
    fn partial_sort_desc_topk_matches_full_sort_prefix_and_set() {
        use engine_state::RouteId;
        let mk = |scores: &[f64]| {
            scores
                .iter()
                .enumerate()
                .map(|(i, &s)| (RouteId(i as u32), s))
                .collect::<Vec<_>>()
        };
        let scores = [3.0, 9.0, 1.0, 7.0, 5.0, 8.0, 2.0, 6.0, 4.0, 10.0];
        let mut reference = mk(&scores);
        reference.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        // n > k: only the top-k need be sorted, and that prefix must be IDENTICAL to
        // the full descending sort's prefix (so the lane1/lane2 split is unchanged).
        let mut got = mk(&scores);
        partial_sort_desc_topk(&mut got, 4);
        assert_eq!(&got[..4], &reference[..4]);

        // n <= k: degrades to a full sort.
        let mut all = mk(&scores);
        partial_sort_desc_topk(&mut all, 20);
        assert_eq!(all, reference);

        // Ties: the top-k SET (by score, descending) is still correct.
        let mut tied = mk(&[5.0, 5.0, 5.0, 1.0, 9.0, 9.0]);
        partial_sort_desc_topk(&mut tied, 3);
        let top3: Vec<f64> = tied[..3].iter().map(|x| x.1).collect();
        assert_eq!(top3, vec![9.0, 9.0, 5.0]);
    }

    #[test]
    fn build_prewarmed_cache_warms_every_pool_at_boot() {
        let (u, p, _permit) = build_family_universe();
        let cache = build_prewarmed_cache(&u);
        assert_eq!(cache.pool_count(), u.pools.len());
        // Rankable families are warmed to a finite rate (not the INFINITY default).
        assert!(cache.get(p.v2, true).is_finite());
        assert!(cache.get(p.v3, true).is_finite());
        // Every pool matches a fresh compute (boot == precompute-on-write at t0).
        for i in 0..u.pools.len() {
            let pool = PoolId(i as u32);
            let [d0, d1] = compute_pool_spot(&u, pool);
            assert_eq!(cache.get(pool, true).to_bits(), d0.to_bits());
            assert_eq!(cache.get(pool, false).to_bits(), d1.to_bits());
        }
    }

    #[test]
    fn refresh_recomputes_cached_rate_from_current_state_precompute_on_write() {
        use engine_state::{V2Reserves256, V2State};
        let (u, p, mut permit) = build_family_universe();
        let cache = prewarm(&u);
        let before = cache.get(p.v2, true);
        assert_eq!(before, v2_spot_after_fee(1000, 2000, 30, true));

        // The state-writer applies a new V2 reserve set (as a Sync log would).
        // WI-06 §3.3 + F1: family-store writes require the sole-writer permit's txn witness — and
        // it must be THIS universe's OWN permit (branded to match its stores), not a foreign one.
        let slot = u.pools.meta(p.v2).state_slot;
        let txn = permit.begin_txn();
        u.v2_state.set(
            slot,
            V2State {
                reserves: V2Reserves256 {
                    reserve0: 1000,
                    reserve1: 4000,
                    block_timestamp_last: 0,
                },
            },
            &txn,
        );
        // Precompute-on-write contract: the cache is STALE until the writer refreshes it.
        assert_eq!(cache.get(p.v2, true), before);

        // The writer refreshes this one pool; the cache now reflects current state.
        refresh_pool_spot(&u, &cache, p.v2);
        let after = cache.get(p.v2, true);
        assert_eq!(after, v2_spot_after_fee(1000, 4000, 30, true));
        assert_ne!(after, before);
        // Other pools are untouched by a single-pool refresh.
        assert_eq!(cache.get(p.v3, true), compute_pool_spot(&u, p.v3)[0]);
    }

    /// WI-06 F1 (codex item 1) RED→GREEN: a permit minted by a DIFFERENT (throwaway) builder
    /// carries a different universe brand, so witnessing THIS universe's resident store with it
    /// is a forgery and must fail-CLOSED (panic). This is the exact out-of-band write the old
    /// `spot_cache` forgery test demonstrated as WORKING; branding makes it unusable.
    #[test]
    #[should_panic(expected = "does not match store universe")]
    fn foreign_universe_permit_cannot_write_resident_state() {
        use engine_state::{V2Reserves256, V2State};
        let (u, p, _permit) = build_family_universe();
        let slot = u.pools.meta(p.v2).state_slot;
        // Forge a permit from an unrelated universe and try to mutate `u`'s store with it.
        let (_, mut forged) = engine_state::UniverseBuilder::new().seal().into_parts();
        let txn = forged.begin_txn();
        u.v2_state.set(
            slot,
            V2State {
                reserves: V2Reserves256 {
                    reserve0: 1,
                    reserve1: 1,
                    block_timestamp_last: 0,
                },
            },
            &txn,
        );
    }

    #[test]
    fn cached_marginal_product_matches_reference_across_families_and_order() {
        let (u, p, _permit) = build_family_universe();
        let cache = prewarm(&u);
        let h = |pool: PoolId, dir: u8| Hop::new(pool, dir);
        // A representative spread of routes, including BOTH sentinel orderings.
        let routes: Vec<Vec<Hop>> = vec![
            vec![h(p.v2, 0), h(p.v2, 1)],             // V2/V2 finite
            vec![h(p.v3, 0), h(p.v3, 1)],             // V3/V3 finite
            vec![h(p.v2, 0), h(p.v3, 1)],             // mixed V2+V3 finite
            vec![h(p.v2, 0), h(p.v3, 0), h(p.v2, 1)], // 3-hop finite
            vec![h(p.solidly, 0), h(p.v2, 1)],        // exotic-first → INFINITY
            vec![h(p.v2, 0), h(p.kyber, 1)],          // exotic-second → INFINITY
            vec![h(p.dead, 0), h(p.v2, 1)],           // dead-first → 0.0
            vec![h(p.v2, 0), h(p.dead, 1)],           // dead-second → 0.0
            vec![h(p.dead, 0), h(p.solidly, 1)],      // dead-before-exotic → 0.0 (order!)
            vec![h(p.solidly, 0), h(p.dead, 1)],      // exotic-before-dead → INFINITY (order!)
        ];
        for hops in &routes {
            let cached = cycle_marginal_product_hops(&cache, hops);
            let reference = reference_marginal_product(&u, hops);
            assert_eq!(
                cached.to_bits(),
                reference.to_bits(),
                "mismatch for route {hops:?}: cached={cached} reference={reference}"
            );
        }
    }

    #[test]
    fn compute_pool_spot_dispatches_every_family_correctly() {
        let (u, p, _permit) = build_family_universe();
        let (v2, v3, solidly, kyber, dead, sqrt2) =
            (p.v2, p.v3, p.solidly, p.kyber, p.dead, p.sqrt2);
        assert_eq!(
            compute_pool_spot(&u, v2),
            [
                v2_spot_after_fee(1000, 2000, 30, true),
                v2_spot_after_fee(1000, 2000, 30, false)
            ]
        );
        assert_eq!(
            compute_pool_spot(&u, v3),
            [
                v3_spot_after_fee(sqrt2, 1_000_000, 3000, true),
                v3_spot_after_fee(sqrt2, 1_000_000, 3000, false)
            ]
        );
        assert_eq!(
            compute_pool_spot(&u, solidly),
            [f64::INFINITY, f64::INFINITY]
        );
        assert_eq!(compute_pool_spot(&u, kyber), [f64::INFINITY, f64::INFINITY]);
        assert_eq!(compute_pool_spot(&u, dead), [0.0, 0.0]);
    }

    #[test]
    fn fold_multiplies_finite_rates_and_is_empty_identity() {
        // All-finite hops fold to the exact bit-for-bit product, in order.
        assert_eq!(fold_marginal_product([1.5, 2.0, 0.5]), 1.5 * 2.0 * 0.5);
        // No hops → identity 1.0 (matches the old `let mut product = 1.0`).
        assert_eq!(fold_marginal_product([]), 1.0);
    }

    #[test]
    fn fold_short_circuits_dead_to_zero_and_exotic_to_infinity() {
        // A dead hop (0.0) culls the whole route regardless of position.
        assert_eq!(fold_marginal_product([2.0, 0.0]), 0.0);
        assert_eq!(fold_marginal_product([0.0, 2.0]), 0.0);
        // An exotic hop (INFINITY) makes the route never-cull regardless of position.
        assert_eq!(fold_marginal_product([2.0, f64::INFINITY]), f64::INFINITY);
        assert_eq!(fold_marginal_product([f64::INFINITY, 2.0]), f64::INFINITY);
    }

    #[test]
    fn fold_preserves_first_sentinel_order_dependence() {
        // THE critical invariant: the old code is a short-circuit fold that returns
        // the FIRST special value in hop order. dead-before-exotic → 0.0;
        // exotic-before-dead → INFINITY. A position-agnostic fold would get this wrong.
        assert_eq!(fold_marginal_product([0.0, f64::INFINITY]), 0.0);
        assert_eq!(fold_marginal_product([f64::INFINITY, 0.0]), f64::INFINITY);
    }

    #[test]
    fn prefetch_is_callable_and_out_of_range_safe() {
        let c = PoolSpotCache::with_pool_count(2);
        // A hint only — no observable effect, but must be bounds-safe (never panics /
        // never computes an out-of-range pointer), including for an unknown pool id.
        c.prefetch(PoolId(0));
        c.prefetch(PoolId(1));
        c.prefetch(PoolId(999));
    }

    #[test]
    fn cache_roundtrips_rates_and_defaults_to_infinity() {
        let c = PoolSpotCache::with_pool_count(3);
        // Default (un-prewarmed) is INFINITY — fail-safe "never cull".
        assert_eq!(c.get(PoolId(0), true), f64::INFINITY);
        assert_eq!(c.get(PoolId(2), false), f64::INFINITY);
        // set() stores the two directions independently, including the 0.0 (dead)
        // sentinel and an ordinary finite rate.
        c.set(PoolId(1), 1.25, 0.0);
        assert_eq!(c.get(PoolId(1), true), 1.25); // dir0
        assert_eq!(c.get(PoolId(1), false), 0.0); // dir1 (dead)
                                                  // The INFINITY sentinel round-trips through the atomic bit store.
        c.set(PoolId(0), f64::INFINITY, 2.0);
        assert_eq!(c.get(PoolId(0), true), f64::INFINITY);
        assert_eq!(c.get(PoolId(0), false), 2.0);
        // Out-of-range pool id reads as INFINITY (fail-safe) and never panics.
        assert_eq!(c.get(PoolId(99), true), f64::INFINITY);
    }
}
