//! Plan §6.1 — `pool_score` computation in 256-bit fixed-point.
//!
//! The score multiplies what we want and divides by what we don't:
//!
//! ```text
//! pool_score = volume_ewma
//!            × price_impact_ewma
//!            × route_degree_score
//!            × historical_hit_rate
//!            × token_quality_score
//!            × protocol_reliability_score
//!            × realized_inclusion_rate
//!            / (cpu_cost
//!               + revert_rate_penalty
//!               + stale_state_penalty
//!               + gas_variance_penalty)
//! ```
//!
//! All inputs are normalised into the canonical Q32.32 fixed-point representation
//! (`u64` with implicit `<< 32` denominator) before composition. The result is bounded:
//! - Numerator factors cap at `2^48` each (caller's responsibility); the running product
//!   over 7 factors fits in `u128` with comfortable headroom.
//! - The denominator is bumped to `1` if all penalty terms are zero so the divisor is
//!   never zero (avoids the bootstrap "no data yet" infinity).
//!
//! The math is integer-only and deterministic, which is what §6 requires — pool scores
//! must be reproducible across cores so atomic tier swaps don't drift between sim
//! workers.

/// Q32.32 fixed-point value (1.0 = `1 << 32`).
pub type Q32_32 = u64;

/// The 11 inputs to `pool_score`, each in Q32.32. Callers (the background scorer) build
/// this struct from EWMA-tracked telemetry and pre-known constants:
///
/// * `volume_ewma` — EWMA inbound flow, normalised so 1.0 = `1 USD/s` of attributable
///   volume (so a $100M/day pool ≈ 1158 USD/s → ~1158 << 32 in Q32.32).
/// * `price_impact_ewma` — EWMA absolute mid-price delta per dirty event, in bps. So a
///   pool that moves 10 bps on average per dirty event has `price_impact_ewma = 10 << 32`.
/// * `route_degree_score` — count of profitable neighbouring routes (cap 1.0).
/// * `historical_hit_rate` — fraction of dirty events producing a positive-EV candidate
///   (0.0 to 1.0).
/// * `token_quality_score` — 1.0 for permissionless, lower for centralised-stable, 0.0
///   for FoT / rebasing / blacklist-callable. A score of 0 excludes the pool entirely.
/// * `protocol_reliability_score` — 1.0 baseline; reduced for V4 hooks, Curve metapools
///   with un-snapshotted base, etc.
/// * `realized_inclusion_rate` — fraction of submitted txs on this pool that were
///   sequenced before competitors (0.0 to 1.0).
/// * `cpu_cost` — p95 sim cost in micro-seconds (then << 32).
/// * `revert_rate_penalty` — REVM-vs-on-chain divergence rate (0.0 to 1.0).
/// * `stale_state_penalty` — fraction of dirty events where local state was stale at sim
///   time (0.0 to 1.0).
/// * `gas_variance_penalty` — stdev of realised gas (in gas units / 10⁵, then << 32).
#[derive(Debug, Clone, Copy)]
pub struct PoolScoreInputs {
    /// EWMA inbound flow, normalised so 1.0 = `1 USD/s` of attributable volume.
    pub volume_ewma: Q32_32,
    /// EWMA absolute mid-price delta per dirty event, in bps.
    pub price_impact_ewma: Q32_32,
    /// Count of profitable neighbouring routes (cap 1.0).
    pub route_degree_score: Q32_32,
    /// Fraction of dirty events producing a positive-EV candidate (0.0 to 1.0).
    pub historical_hit_rate: Q32_32,
    /// 1.0 for permissionless, lower for centralised-stable, 0.0 for risky tokens.
    pub token_quality_score: Q32_32,
    /// 1.0 baseline; reduced for V4 hooks, Curve metapools with un-snapshotted base, etc.
    pub protocol_reliability_score: Q32_32,
    /// Fraction of submitted txs on this pool that were sequenced before competitors.
    pub realized_inclusion_rate: Q32_32,
    /// Structural route-winnability multiplier (§6.1 hub→LST re-weight). Tilts the
    /// hot-tier election toward less-contested, persistent flow we can actually land
    /// first: LST↔{WETH,LST} pairs are boosted, pure hub (WETH/stable, stable/stable)
    /// pairs are de-emphasized when not co-located. Seeded once from the token pair at
    /// boot (structural, not telemetry); the live `realized_inclusion_rate` term then
    /// self-corrects toward whatever we are actually filling. Unit (1.0) = neutral.
    pub route_winnability: Q32_32,
    /// p95 sim cost in micro-seconds (Q32.32).
    pub cpu_cost: Q32_32,
    /// REVM-vs-on-chain divergence rate (0.0 to 1.0).
    pub revert_rate_penalty: Q32_32,
    /// Fraction of dirty events where local state was stale at sim time.
    pub stale_state_penalty: Q32_32,
    /// Stdev of realised gas (gas-units / 10⁵, Q32.32).
    pub gas_variance_penalty: Q32_32,
}

impl PoolScoreInputs {
    /// Bootstrap value: every numerator factor = 1, every denominator factor = 0. With
    /// the safety bump-to-1 on the denominator, this returns score = 1.0 (`1 << 32`).
    pub fn unit_baseline() -> Self {
        let one = 1u64 << 32;
        Self {
            volume_ewma: one,
            price_impact_ewma: one,
            route_degree_score: one,
            historical_hit_rate: one,
            token_quality_score: one,
            protocol_reliability_score: one,
            realized_inclusion_rate: one,
            route_winnability: one,
            cpu_cost: 0,
            revert_rate_penalty: 0,
            stale_state_penalty: 0,
            gas_variance_penalty: 0,
        }
    }
}

/// Outputs from `score_pool` — both the headline `pool_score` and the intermediate
/// numerator/denominator so callers can debug / log the breakdown without re-deriving.
#[derive(Debug, Clone, Copy)]
pub struct PoolScoreOutputs {
    /// Final pool score in Q32.32.
    pub pool_score: u64,
    /// Numerator product before division (u128).
    pub numerator: u128,
    /// Denominator sum before division (u128, always >= 1).
    pub denominator: u128,
}

/// Compute `pool_score` from a `PoolScoreInputs`. Returns score in Q32.32, plus the
/// intermediate numerator / denominator for telemetry.
///
/// All Q32.32 multiplications are widened to u128 to keep precision; the final divide
/// produces a Q32.32 result by *not* re-shifting (the seven Q32.32 multiplications leave
/// the running product at Q32.32 * Q0.32^6 = Q32.224, but we want Q32.32 → divide by
/// `1 << (32 * 6) = 1 << 192`). Since `denominator` is Q32.32 too, the final ratio sits
/// at Q32.32 once we cancel one of the `<< 32` factors. The implementation evaluates this
/// as `(numerator >> 192) << 32 / denominator` to keep the shifts cheap.
///
/// Token-quality = 0 → score = 0 (the pool is excluded by §6.1 explicitly).
pub fn score_pool(inputs: &PoolScoreInputs) -> PoolScoreOutputs {
    if inputs.token_quality_score == 0 {
        return PoolScoreOutputs {
            pool_score: 0,
            numerator: 0,
            denominator: 1,
        };
    }
    // Numerator: chain of 7 Q32.32 multiplications. After `n` multiplications the value
    // sits at Q32.(32n); we keep it in u128 and shift down at the end.
    let mut n: u128 = inputs.volume_ewma as u128;
    n = (n * inputs.price_impact_ewma as u128) >> 32;
    n = (n * inputs.route_degree_score as u128) >> 32;
    n = (n * inputs.historical_hit_rate as u128) >> 32;
    n = (n * inputs.token_quality_score as u128) >> 32;
    n = (n * inputs.protocol_reliability_score as u128) >> 32;
    n = (n * inputs.realized_inclusion_rate as u128) >> 32;
    n = (n * inputs.route_winnability as u128) >> 32;

    // Denominator: sum of penalties; bump to 1 if zero so we never divide by zero.
    let d: u128 = (inputs.cpu_cost as u128)
        + (inputs.revert_rate_penalty as u128)
        + (inputs.stale_state_penalty as u128)
        + (inputs.gas_variance_penalty as u128);
    let denominator = if d == 0 { 1u128 << 32 } else { d };

    // n is Q32.32; denominator is Q32.32. Ratio in Q32.32 = (n << 32) / denominator.
    let pool_score_u128 = (n << 32) / denominator;
    // Cap at u64::MAX so the score fits the published table format.
    let pool_score = pool_score_u128.min(u64::MAX as u128) as u64;

    PoolScoreOutputs {
        pool_score,
        numerator: n,
        denominator,
    }
}

/// Token role for the §6.1 route-winnability re-weight, classified once at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRole {
    /// ETH-correlated liquid staking token (wstETH, weETH, rETH, ezETH, sfrxETH).
    Lst,
    /// WETH itself (the ETH-correlated anchor).
    Weth,
    /// USD-pegged stablecoin.
    Stable,
    /// Everything else (volatile majors and long-tail).
    Other,
}

/// Structural route-winnability multiplier (Q32.32) for a pool, from its two token
/// roles. Encodes the §6.1 hub→LST tilt:
/// - both legs ETH-correlated (LST↔{WETH,LST}) → ×4: persistent, under-contested flow
///   driven by yield accrual / redemption queues — winnable even when we're slow.
/// - exactly one LST leg → ×2.
/// - pure hub (WETH/stable, stable/stable) → ×0.5 when *not* co-located (we lose those
///   FCFS races), lifted to ×1.0 when `colocated` (AWS, sub-ms: contest them too).
/// - anything else → ×1.0 (neutral).
///
/// This is the cold-start prior only; once live, `realized_inclusion_rate` rebalances
/// toward whatever is actually being filled (so hubs re-enter the hot tier on their own
/// once we start landing them).
#[inline]
pub fn route_winnability_q32(a: TokenRole, b: TokenRole, colocated: bool) -> u64 {
    use TokenRole::*;
    let eth_corr = |r: TokenRole| matches!(r, Lst | Weth);
    let major = |r: TokenRole| matches!(r, Lst | Weth | Stable);
    if eth_corr(a) && eth_corr(b) {
        4u64 << 32
    } else if matches!(a, Lst) || matches!(b, Lst) {
        2u64 << 32
    } else if major(a) && major(b) {
        if colocated {
            1u64 << 32
        } else {
            1u64 << 31 // 0.5
        }
    } else {
        1u64 << 32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one() -> u64 {
        1u64 << 32
    }

    #[test]
    fn unit_baseline_scores_one() {
        let r = score_pool(&PoolScoreInputs::unit_baseline());
        assert_eq!(r.pool_score, one());
    }

    #[test]
    fn route_winnability_reweights_lst_above_hub() {
        use TokenRole::*;
        // Same base score; only the winnability factor differs.
        let mut lst = PoolScoreInputs::unit_baseline();
        lst.volume_ewma = 100 * one();
        let mut hub = lst;
        lst.route_winnability = route_winnability_q32(Lst, Weth, false); // ×4
        hub.route_winnability = route_winnability_q32(Weth, Stable, false); // ×0.5
        let rl = score_pool(&lst);
        let rh = score_pool(&hub);
        // LST/WETH should outrank a deeper-but-contested hub by ~8x (4 / 0.5).
        assert!(rl.pool_score > rh.pool_score);
        assert_eq!(rl.pool_score / rh.pool_score, 8);
    }

    #[test]
    fn route_winnability_colocation_lifts_hub_penalty() {
        use TokenRole::*;
        // Co-located: the contested-hub de-emphasis is lifted to neutral.
        assert_eq!(route_winnability_q32(Weth, Stable, true), one());
        assert_eq!(route_winnability_q32(Stable, Stable, true), one());
        // Lagged: hub is halved.
        assert_eq!(route_winnability_q32(Weth, Stable, false), one() / 2);
        // LST flow is boosted in both regimes.
        assert_eq!(route_winnability_q32(Lst, Lst, true), 4 * one());
        assert_eq!(route_winnability_q32(Lst, Stable, false), 2 * one());
        // Neutral pairs untouched.
        assert_eq!(route_winnability_q32(Other, Weth, false), one());
        assert_eq!(route_winnability_q32(Other, Stable, false), one());
    }

    #[test]
    fn zero_token_quality_excludes() {
        let mut i = PoolScoreInputs::unit_baseline();
        i.token_quality_score = 0;
        let r = score_pool(&i);
        assert_eq!(r.pool_score, 0);
    }

    #[test]
    fn higher_volume_higher_score() {
        let mut a = PoolScoreInputs::unit_baseline();
        let mut b = PoolScoreInputs::unit_baseline();
        a.volume_ewma = 100 * one();
        b.volume_ewma = 1000 * one();
        let ra = score_pool(&a);
        let rb = score_pool(&b);
        assert!(rb.pool_score > ra.pool_score);
        assert!(rb.pool_score / ra.pool_score >= 9 && rb.pool_score / ra.pool_score <= 11);
    }

    #[test]
    fn higher_cpu_cost_lower_score() {
        let mut a = PoolScoreInputs::unit_baseline();
        a.volume_ewma = 100 * one();
        let mut b = a;
        b.cpu_cost = 100 * one();
        let ra = score_pool(&a);
        let rb = score_pool(&b);
        assert!(rb.pool_score < ra.pool_score);
    }

    #[test]
    fn higher_revert_rate_lower_score() {
        let mut a = PoolScoreInputs::unit_baseline();
        a.volume_ewma = 100 * one();
        let mut b = a;
        b.revert_rate_penalty = 50 * one();
        let ra = score_pool(&a);
        let rb = score_pool(&b);
        assert!(rb.pool_score < ra.pool_score);
    }

    #[test]
    fn no_division_by_zero_at_boot() {
        // Every input zero except token_quality = 1 must produce score 0 (because the
        // numerator is zero), not panic / divide-by-zero.
        let i = PoolScoreInputs {
            volume_ewma: 0,
            price_impact_ewma: 0,
            route_degree_score: 0,
            historical_hit_rate: 0,
            token_quality_score: one(),
            protocol_reliability_score: 0,
            realized_inclusion_rate: 0,
            route_winnability: 0,
            cpu_cost: 0,
            revert_rate_penalty: 0,
            stale_state_penalty: 0,
            gas_variance_penalty: 0,
        };
        let r = score_pool(&i);
        assert_eq!(r.pool_score, 0);
    }
}
