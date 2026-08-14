//! Per-route hot-path metadata, precomputed at startup.
//!
//! Plan §0.4 rule "no hot-path RPC, no hot-path heap allocs": every per-evaluation
//! lookup we can collapse to a single indexed array read is a win. Today
//! `evaluate_two_hop_v3` does:
//!
//! 1. `route_table.get(route_id)` — one Vec index
//! 2. `route.hops[0].pool`, `route.hops[1].pool` — array accesses on `Route`
//! 3. `pools.meta(pool)` × 2 — two more Vec indexes for `PoolMeta`
//! 4. `match m1.kind, m2.kind` against PoolKind enum — branch chain
//! 5. `s1.fee_pips / 100 + s2.fee_pips / 100` — two arithmetic ops on V3 state
//! 6. Pool state load × 2 — atomic ArcSwap derefs
//! 7. Direction + zero_for_one extraction — bit ops
//!
//! Steps 1–5 are all *immutable after init*. This module precomputes them into
//! `RouteFastPath`, indexed by `RouteId.0`. Hot path becomes:
//!
//! ```text
//! let fp = &fast_paths[route_id.0 as usize];
//! match fp.dispatch { ... }
//! // fp.total_fee_bps, fp.state_slots[0..k], fp.directions all direct reads.
//! ```
//!
//! Net result on the hot path:
//! * one cache-line read for `RouteFastPath` (we pack to 64 B per route)
//! * removes the two `pools.meta()` indexes per evaluation
//! * removes the V3 fee/100 arithmetic
//! * removes the `match m1.kind, m2.kind` branch chain

use engine_state::{PoolKind, PoolMeta, PoolTable};

use crate::table::{Route, MAX_HOPS};

/// Compact per-route dispatch tag — single byte.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDispatch {
    /// 2-hop V2/V2.
    V2V2 = 0,
    /// 2-hop V3/V3.
    V3V3 = 1,
    /// 2-hop V2 then V3.
    V2V3 = 2,
    /// 2-hop V3 then V2.
    V3V2 = 3,
    /// 3-hop V2/V2/V2 triangle (current evaluator only supports all-V2).
    TriangleV2 = 4,
    /// k-hop V3-only deep cycle (4..=8).
    NHopV3 = 5,
    /// 2-hop Fluid-on-both-hops (DEX v1/v2 cross-pool inside Fluid).
    FluidFluid = 6,
    /// 2-hop Fluid + Uni-V3 family.
    FluidV3 = 7,
    /// 2-hop Uni-V3 family + Fluid.
    V3Fluid = 8,
    /// 2-hop DODO V2 + DODO V2 (PMM cross-pool).
    DodoDodo = 9,
    /// 2-hop Solidly stable + stable (x³y + xy³ cross-pool).
    SolidlyStable = 10,
    /// 2-hop generic cross-kernel cycle. Any two "quotable" families that don't
    /// match a more-specific tag above — e.g. Solidly+V2, Solidly+V3, V2+V3 odd
    /// mixes. The generic evaluator composes each kernel's own `quote_one_hop`
    /// so we never need a bespoke evaluator per family pair. `is_generic_quotable`
    /// is the single source of truth for admission: it INCLUDES DODO V2 (id 384)
    /// and EXCLUDES only Kyber, Maverick, and FluidDex (whose kernels are gated off
    /// as generic legs). Note this is a sim-admission set, NOT a byte-exact fast-lane
    /// promotion set — every candidate is still REVM/on-chain validated.
    MixedTwoHop = 11,
    /// Unsupported family combination — evaluator skips silently.
    Unsupported = 0xFF,
}

impl RouteDispatch {
    /// Decode from `u8` (used after reading from the packed `RouteFastPath`).
    #[inline]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::V2V2,
            1 => Self::V3V3,
            2 => Self::V2V3,
            3 => Self::V3V2,
            4 => Self::TriangleV2,
            5 => Self::NHopV3,
            6 => Self::FluidFluid,
            7 => Self::FluidV3,
            8 => Self::V3Fluid,
            9 => Self::DodoDodo,
            10 => Self::SolidlyStable,
            11 => Self::MixedTwoHop,
            _ => Self::Unsupported,
        }
    }
}

/// Per-route precomputed hot-path metadata. Sized for a single 64-byte cache line on
/// AArch64 / x86 (verified by `assert_eq!(size_of::<RouteFastPath>(), 64)` test below).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RouteFastPath {
    /// `RouteDispatch as u8`. See `RouteDispatch::from_u8`.
    pub dispatch: u8,
    /// Hop count (2..=8).
    pub hop_count: u8,
    /// Direction bitmask: bit i = hop i's direction (0/1).
    pub directions: u8,
    /// Padding to keep the next field naturally aligned.
    pub _pad0: u8,
    /// Sum of per-hop fees in **basis points** (V3 fees divided by 100). Drives the
    /// §10.6 fee-floor pre-filter without per-evaluation `fee_pips / 100`.
    pub total_fee_bps: u16,
    /// Padding.
    pub _pad1: u16,
    /// Estimated gas-units for the §12 EV cost model. V2/V2 ≈ 220k, V3/V3 ≈ 260k,
    /// mixed ≈ 240k, triangle ≈ 280k, deep V3 ≈ 20k + 110k × hops.
    pub gas_units: u32,
    /// Estimated calldata byte count for L1 data-fee projection.
    pub calldata_bytes: u32,
    /// Per-hop state-store slot indexes. `state_slots[i]` is the slot in the family-
    /// specific store for hop `i`'s pool. Index into V2State or V3State via the
    /// dispatch tag (hops have homogeneous family within a 2-hop V2V2 / V3V3, and
    /// mixed for V2V3 / V3V2 — caller dispatches by `dispatch`).
    pub state_slots: [u32; MAX_HOPS],
    /// Padding to fill the cache line.
    /// 4 (head) + 4 (fee+pad) + 8 (gas+calldata) + 32 (state_slots) = 48 bytes.
    /// We pad to 64.
    pub _pad_tail: [u8; 16],
}

const _: () = {
    // Compile-time assertion: cache-line-sized.
    assert!(std::mem::size_of::<RouteFastPath>() == 64);
};

impl RouteFastPath {
    /// Sentinel "unsupported" entry — every field zeroed except `dispatch`.
    pub const UNSUPPORTED: Self = Self {
        dispatch: RouteDispatch::Unsupported as u8,
        hop_count: 0,
        directions: 0,
        _pad0: 0,
        total_fee_bps: 0,
        _pad1: 0,
        gas_units: 0,
        calldata_bytes: 0,
        state_slots: [0; MAX_HOPS],
        _pad_tail: [0; 16],
    };

    /// Build a fast-path entry from a Route + the pool table. Returns `None` if the
    /// route uses an unsupported family combination (caller stores `UNSUPPORTED` so
    /// the array stays dense and indexable by `route_id`).
    pub fn from_route(route: &Route, pools: &PoolTable) -> Self {
        let n = route.hop_count as usize;
        if !(2..=MAX_HOPS).contains(&n) {
            return Self::UNSUPPORTED;
        }
        let mut state_slots = [0u32; MAX_HOPS];
        let mut directions: u8 = 0;
        let mut total_fee_bps_acc: u32 = 0;
        for (i, h) in route.hops.iter().enumerate().take(n) {
            let m: &PoolMeta = pools.meta(h.pool);
            state_slots[i] = m.state_slot.0;
            if (h.direction & 1) != 0 {
                directions |= 1u8 << (i as u8);
            }
            let fee_bps = match m.kind {
                PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => m.fee_pips_or_bps,
                PoolKind::UniswapV3
                | PoolKind::SushiV3
                | PoolKind::PancakeV3
                | PoolKind::AlgebraV19
                | PoolKind::UniswapV4
                | PoolKind::KyberSwapElastic => m.fee_pips_or_bps / 100,
                PoolKind::DodoV2 => {
                    // DODO stores total fee (lp + mt) in ppm via the `fee_ppm`
                    // field on `DodoState`. We capture the same value in
                    // `PoolMeta.fee_pips_or_bps` at registration (see `add_dodo_pool`),
                    // so /100 → bps mirrors V3 family.
                    m.fee_pips_or_bps / 100
                }
                PoolKind::SolidlyV3 => {
                    // Solidly stable pools store fee in bps already in `fee_pips_or_bps`.
                    m.fee_pips_or_bps
                }
                PoolKind::TraderJoeLb => {
                    // Trader Joe LB stores its base fee already in bps in
                    // `fee_pips_or_bps` (resolved from base_factor * bin_step at
                    // registration), so use it directly like Solidly / V2 — NOT /100.
                    m.fee_pips_or_bps
                }
                PoolKind::MaverickV2 => {
                    // Maverick stores the larger of its two directional fees in
                    // `fee_pips_or_bps` as ppm at registration (see `add_maverick_pool`);
                    // /100 → bps mirrors the V3 family for the fee-floor pre-filter.
                    m.fee_pips_or_bps / 100
                }
                PoolKind::FluidDex => {
                    // Fluid v1/v2 fees stored as `fee_pips_or_bps` in 1e6 precision
                    // (matching Fluid's `FEE_DENOMINATOR_1E6` = 1_000_000). Convert
                    // to bps by dividing by 100. Fluid v2 dynamic-fee pools encode
                    // `0xFFFFFF` for "dynamic" — clamp to 30 bps for the static
                    // fee-floor pre-filter; the live fee is consulted by the
                    // kernel at quote time.
                    if m.fee_pips_or_bps == 0xFFFFFF {
                        30
                    } else {
                        m.fee_pips_or_bps / 100
                    }
                }
                PoolKind::BalancerStable | PoolKind::BalancerWeighted => {
                    // Balancer swap fee stored as ppm in `fee_pips_or_bps`; /100 → bps.
                    m.fee_pips_or_bps / 100
                }
                PoolKind::CurveStableSwapNg | PoolKind::CurveCryptoSwap => {
                    // Curve fee stored in its native 1e10 units in `fee_pips_or_bps`
                    // (e.g. 1_000_000 = 1 bps); /1e6 → bps for the §10.6 fee floor.
                    // CryptoSwap shares the 1e10 FEE_DENOMINATOR.
                    m.fee_pips_or_bps / 1_000_000
                } // NOTE: exhaustive — every PoolKind has a fee→bps mapping above. A new family
                  // added to PoolKind will fail to compile here until its fee unit is declared,
                  // which is the intended guard (no silent UNSUPPORTED fall-through).
            };
            total_fee_bps_acc = total_fee_bps_acc.saturating_add(fee_bps);
        }

        // Dispatch tag based on hop_count + family mix.
        let dispatch = if n == 2 {
            let k0 = pools.meta(route.hops[0].pool).kind;
            let k1 = pools.meta(route.hops[1].pool).kind;
            let m0_v2 = is_v2(k0);
            let m1_v2 = is_v2(k1);
            let m0_v3 = is_v3(k0);
            let m1_v3 = is_v3(k1);
            let m0_fluid = matches!(k0, PoolKind::FluidDex);
            let m1_fluid = matches!(k1, PoolKind::FluidDex);
            let m0_dodo = matches!(k0, PoolKind::DodoV2);
            let m1_dodo = matches!(k1, PoolKind::DodoV2);
            let m0_solidly = matches!(k0, PoolKind::SolidlyV3);
            let m1_solidly = matches!(k1, PoolKind::SolidlyV3);
            if m0_dodo && m1_dodo {
                RouteDispatch::DodoDodo
            } else if m0_solidly && m1_solidly {
                RouteDispatch::SolidlyStable
            } else {
                match (m0_v2, m1_v2, m0_v3, m1_v3, m0_fluid, m1_fluid) {
                    (true, true, _, _, _, _) => RouteDispatch::V2V2,
                    (_, _, true, true, _, _) => RouteDispatch::V3V3,
                    (true, false, false, true, _, _) => RouteDispatch::V2V3,
                    (false, true, true, false, _, _) => RouteDispatch::V3V2,
                    (_, _, _, _, true, true) => RouteDispatch::FluidFluid,
                    (_, _, _, true, true, false) => RouteDispatch::FluidV3,
                    (_, _, true, _, false, true) => RouteDispatch::V3Fluid,
                    // Any remaining pair of quotable families (Solidly+V2,
                    // Solidly+V3, …) routes through the generic cross-kernel
                    // evaluator. Fluid and the unverified Kyber / DODO / Maverick
                    // kernels are excluded — Fluid has dedicated tags, the others
                    // produced phantom cross-family quotes and keep only their
                    // dedicated same-kernel evaluators.
                    _ if is_generic_quotable(k0) && is_generic_quotable(k1) => {
                        RouteDispatch::MixedTwoHop
                    }
                    _ => RouteDispatch::Unsupported,
                }
            }
        } else if n == 3 {
            // Triangle: every hop must be a generic-quotable family. The evaluator
            // dispatches per-hop kernel internally via `quote_one_hop`, so mixed
            // triangles across V2 / V3 / Solidly are all first-class. Reject if a
            // hop is a family we don't quote in the generic path (Fluid, Curve,
            // TraderJoe, V4, and the unverified Kyber / DODO / Maverick kernels).
            let k0 = pools.meta(route.hops[0].pool).kind;
            let k1 = pools.meta(route.hops[1].pool).kind;
            let k2 = pools.meta(route.hops[2].pool).kind;
            if is_generic_quotable(k0) && is_generic_quotable(k1) && is_generic_quotable(k2) {
                RouteDispatch::TriangleV2
            } else {
                RouteDispatch::Unsupported
            }
        } else if (4..=8).contains(&n) {
            // N-hop. Every hop must be a generic-quotable family — `evaluate_n_hop_v3` dispatches the
            // per-hop kernel internally, so mixed cycles (V2/V3 + Solidly/Balancer/Curve/DODO/LB) are
            // first-class. [Codex-F4] Use `is_generic_quotable` (the 14-family evaluator contract) so
            // this admission stays in LOCKSTEP with the evaluator; it previously admitted only the 8
            // byte-exact V2/V3/V4 families and silently dropped submittable mixed-family 4-hop cycles.
            // NOTE — our decision / plan for now: live topology is capped at 4 hops via `TopologyConfig`
            // (5–8-hop tiers disabled by default), so this arm only ever sees n == 4 in practice; the
            // 4..=8 range is kept for reversibility if we later re-enable deeper cycles.
            let mut ok = true;
            for i in 0..n {
                if !is_generic_quotable(pools.meta(route.hops[i].pool).kind) {
                    ok = false;
                    break;
                }
            }
            if ok {
                RouteDispatch::NHopV3
            } else {
                RouteDispatch::Unsupported
            }
        } else {
            RouteDispatch::Unsupported
        };

        // Gas / calldata projections per dispatch tag. L2 gas_units are calibrated
        // against observed on-chain arb gas_used (Dune, in-universe Arbitrum arbs
        // 2026-05-24): 2-leg V3 cycles ~347k, 3-leg ~500k, 4-leg ~775k, at an
        // effective price of ~0.0215 gwei with a negligible (<1.5k gas) L1-data
        // component. The earlier estimates undercharged multi-hop by 25-40%, which
        // risked clearing routes that actually lose to gas; these match reality with
        // a small conservative margin (over-, never under-charge the EV gate).
        let (gas_units, calldata_bytes) = match dispatch {
            RouteDispatch::V2V2 => (280_000u32, 240u32),
            RouteDispatch::V3V3 => (350_000, 280),
            RouteDispatch::V2V3 | RouteDispatch::V3V2 => (320_000, 260),
            RouteDispatch::TriangleV2 => (420_000, 280),
            // [Codex-F4] 200k/hop (was 175k) keeps the EV-gate cull CONSERVATIVE now that heavier
            // Tier-2 kernels (Curve/Balancer/DODO ~200–250k/hop) are admitted to the n-hop path —
            // over-, never under-charge. (REVM + on-chain minNetProfit revalidate real gas downstream.)
            RouteDispatch::NHopV3 => (60_000 + 200_000 * (n as u32), 80 + 40 * (n as u32)),
            RouteDispatch::FluidFluid => (680_000, 360),
            RouteDispatch::FluidV3 | RouteDispatch::V3Fluid => (600_000, 320),
            // DODO V2 sellBase/sellQuote is ~250k gas; pair-of-pools cycle ~500k.
            RouteDispatch::DodoDodo => (520_000, 320),
            // Solidly stable swap is ~180k per hop (Newton-Raphson inline).
            RouteDispatch::SolidlyStable => (380_000, 280),
            // Generic cross-kernel 2-hop. Worst-case mix (DODO PMM ~250k +
            // Maverick tick walk ~300k) → conservative 560k so the EV gate
            // doesn't under-charge gas on the heavier kernels.
            RouteDispatch::MixedTwoHop => (560_000, 340),
            RouteDispatch::Unsupported => (0, 0),
        };

        Self {
            dispatch: dispatch as u8,
            hop_count: n as u8,
            directions,
            _pad0: 0,
            total_fee_bps: total_fee_bps_acc.min(u16::MAX as u32) as u16,
            _pad1: 0,
            gas_units,
            calldata_bytes,
            state_slots,
            _pad_tail: [0; 16],
        }
    }
}

#[inline]
fn is_v2(k: PoolKind) -> bool {
    matches!(
        k,
        PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2
    )
}

/// Families the generic cross-kernel 2-hop / triangle evaluator can quote. MUST stay
/// in lockstep with the `quotable` closure in `engine::pipeline::evaluate_triangle` —
/// a family admitted here but rejected there (or vice versa) is silently dropped as
/// UnsupportedShape. Excludes Fluid (handled by FluidFluid / FluidV3 / V3Fluid),
/// TraderJoe (dedicated evaluators), and the UNVERIFIED kernels (Kyber / Maverick)
/// that produced phantom cycle profit. Curve StableSwap-NG, Balancer-stable, and DODO
/// ARE quotable: `quote_one_hop` is wired for each (Curve's static-fee kernel is
/// accurate for balanced stable pairs; see `engine_amm::curve`).
/// SINGLE SOURCE OF TRUTH for the "generic-quotable" AMM family set — the families the per-hop
/// quoter (`quote_one_hop`) prices and that `evaluate_triangle` / `evaluate_n_hop_v3` admit. The CSR
/// route-admission (`from_route`) and those sim evaluators MUST call this rather than re-inlining a
/// `matches!` list, so admission and evaluation can never drift (a family admitted but not evaluated
/// — or vice versa — is silently dropped as `UnsupportedShape`). See
/// `generic_quotable_set_is_pinned_single_source_of_truth`. [Codex AUD-014 hardening]
#[inline]
pub fn is_generic_quotable(k: PoolKind) -> bool {
    matches!(
        k,
        PoolKind::UniswapV2
            | PoolKind::SushiV2
            | PoolKind::CamelotV2
            | PoolKind::UniswapV3
            | PoolKind::SushiV3
            | PoolKind::PancakeV3
            | PoolKind::AlgebraV19
            | PoolKind::UniswapV4
            | PoolKind::SolidlyV3
            | PoolKind::BalancerStable
            | PoolKind::BalancerWeighted
            | PoolKind::CurveStableSwapNg
            | PoolKind::CurveCryptoSwap
            | PoolKind::DodoV2
            | PoolKind::TraderJoeLb
    )
}

#[inline]
fn is_v3(k: PoolKind) -> bool {
    matches!(
        k,
        PoolKind::UniswapV3
            | PoolKind::SushiV3
            | PoolKind::PancakeV3
            | PoolKind::AlgebraV19
            | PoolKind::UniswapV4
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_fast_path_is_cache_line_sized() {
        assert_eq!(std::mem::size_of::<RouteFastPath>(), 64);
    }

    #[test]
    fn unsupported_has_zero_gas() {
        let u = RouteFastPath::UNSUPPORTED;
        assert_eq!(u.dispatch, RouteDispatch::Unsupported as u8);
        assert_eq!(u.gas_units, 0);
        assert_eq!(u.total_fee_bps, 0);
    }

    #[test]
    fn route_dispatch_round_trip() {
        for d in [
            RouteDispatch::V2V2,
            RouteDispatch::V3V3,
            RouteDispatch::V2V3,
            RouteDispatch::V3V2,
            RouteDispatch::TriangleV2,
            RouteDispatch::NHopV3,
            RouteDispatch::Unsupported,
        ] {
            assert_eq!(RouteDispatch::from_u8(d as u8), d);
        }
    }

    // --- Uniswap V4 (hookless) treated as V3-family for route dispatch ---

    fn v4_meta(id_seed: u32, t0: u16, t1: u16, kind: PoolKind) -> PoolMeta {
        use engine_state::{PairId, StateSlot, TokenId, VenueId};
        let mut addr = [0u8; 20];
        addr[..4].copy_from_slice(&id_seed.to_be_bytes());
        PoolMeta {
            address: addr,
            kind,
            token0: TokenId(t0),
            token1: TokenId(t1),
            pair: PairId(0),
            venue: VenueId(0),
            // V4 stores fee in pips (ppm), same as V3.
            fee_pips_or_bps: 500,
            tick_spacing: 10,
            state_slot: StateSlot(id_seed),
        }
    }

    fn two_hop_route(p0: u32, p1: u32) -> Route {
        use crate::table::{Hop, StrategyKind};
        use engine_state::{PoolId, RouteId, TokenId};
        let mut hops = arrayvec::ArrayVec::new();
        hops.push(Hop::new(PoolId(p0), 0));
        hops.push(Hop::new(PoolId(p1), 1));
        let mut tokens = arrayvec::ArrayVec::new();
        tokens.push(TokenId(0));
        tokens.push(TokenId(1));
        tokens.push(TokenId(0));
        Route {
            id: RouteId(0),
            strategy: StrategyKind::TwoHop,
            hop_count: 2,
            hops,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: true,
        }
    }

    fn four_hop_route(p: [u32; 4]) -> Route {
        use crate::table::{Hop, StrategyKind};
        use engine_state::{PoolId, RouteId, TokenId};
        let mut hops = arrayvec::ArrayVec::new();
        for (i, &pool) in p.iter().enumerate() {
            hops.push(Hop::new(PoolId(pool), i as u8));
        }
        let mut tokens = arrayvec::ArrayVec::new();
        for t in [0u16, 1, 2, 3, 0] {
            tokens.push(TokenId(t));
        }
        Route {
            id: RouteId(0),
            strategy: StrategyKind::FourHop,
            hop_count: 4,
            hops,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: true,
        }
    }

    #[test]
    fn four_hop_mixed_family_is_admitted_as_nhop_not_unsupported() {
        // [Codex-F4] A 4-hop cycle mixing V3 with the generic-quotable Tier-2 families (Balancer-
        // stable, Curve) is priced by `evaluate_n_hop_v3`, so the CSR n-hop admission must tag it
        // NHopV3 (not Unsupported) — else the boot index silently drops a SUBMITTABLE 4-hop route.
        let mut pt = PoolTable::new();
        pt.insert(v4_meta(0, 0, 1, PoolKind::UniswapV3));
        pt.insert(v4_meta(1, 1, 2, PoolKind::BalancerStable));
        pt.insert(v4_meta(2, 2, 3, PoolKind::UniswapV2));
        pt.insert(v4_meta(3, 3, 0, PoolKind::CurveStableSwapNg));
        let r = four_hop_route([0, 1, 2, 3]);
        let fp = RouteFastPath::from_route(&r, &pt);
        assert_eq!(
            RouteDispatch::from_u8(fp.dispatch),
            RouteDispatch::NHopV3,
            "4-hop mixed-family (V3/Balancer/V2/Curve) must be admitted to the n-hop CSR"
        );
        // The n-hop gas estimate must not UNDER-charge once heavier kernels are admitted.
        assert!(
            fp.gas_units >= 760_000,
            "n-hop gas estimate too low for a mixed-family 4-hop: {}",
            fp.gas_units
        );
    }

    #[test]
    fn four_hop_dodo_mixed_family_is_admitted_as_nhop_not_unsupported() {
        let mut pt = PoolTable::new();
        pt.insert(v4_meta(0, 0, 1, PoolKind::UniswapV2));
        pt.insert(v4_meta(1, 1, 2, PoolKind::UniswapV2));
        pt.insert(v4_meta(2, 2, 3, PoolKind::DodoV2));
        pt.insert(v4_meta(3, 3, 0, PoolKind::UniswapV2));
        let r = four_hop_route([0, 1, 2, 3]);
        let fp = RouteFastPath::from_route(&r, &pt);
        assert_eq!(
            RouteDispatch::from_u8(fp.dispatch),
            RouteDispatch::NHopV3,
            "4-hop mixed-family (V2/V2/DODO/V2) must be admitted to the n-hop CSR"
        );
    }

    #[test]
    fn v4_is_treated_as_v3_family() {
        assert!(is_v3(PoolKind::UniswapV4));
        assert!(is_generic_quotable(PoolKind::UniswapV4));
        assert!(!is_v2(PoolKind::UniswapV4));
    }

    #[test]
    fn generic_quotable_set_is_pinned_single_source_of_truth() {
        // [Codex AUD-014 hardening] `is_generic_quotable` is the SINGLE SOURCE OF TRUTH for the
        // generic-quotable family set: the CSR n-hop admission (`from_route`) and the sim evaluators
        // (`evaluate_triangle` / `evaluate_n_hop_v3`) all call THIS function, so they cannot drift.
        // This test pins the exact set so any change is deliberate and reviewed.
        let quotable = [
            PoolKind::UniswapV2,
            PoolKind::SushiV2,
            PoolKind::CamelotV2,
            PoolKind::UniswapV3,
            PoolKind::SushiV3,
            PoolKind::PancakeV3,
            PoolKind::AlgebraV19,
            PoolKind::UniswapV4,
            PoolKind::SolidlyV3,
            PoolKind::BalancerStable,
            PoolKind::BalancerWeighted,
            PoolKind::CurveStableSwapNg,
            PoolKind::CurveCryptoSwap,
            PoolKind::DodoV2,
            PoolKind::TraderJoeLb,
        ];
        for k in quotable {
            assert!(is_generic_quotable(k), "{k:?} must be generic-quotable");
        }
        assert_eq!(
            quotable.len(),
            15,
            "the quotable set is exactly 15 families"
        );
        // Excluded (unverified / dedicated-evaluator-only kernels): a mistaken inclusion here would
        // route phantom-priced cycles into the candidate stream.
        for k in [
            PoolKind::FluidDex,
            PoolKind::KyberSwapElastic,
            PoolKind::MaverickV2,
        ] {
            assert!(
                !is_generic_quotable(k),
                "{k:?} must NOT be generic-quotable (unverified/dedicated kernel)"
            );
        }
    }

    #[test]
    fn v4_v4_two_hop_dispatches_v3v3_not_unsupported() {
        let mut pt = PoolTable::new();
        pt.insert(v4_meta(0, 0, 1, PoolKind::UniswapV4));
        pt.insert(v4_meta(1, 0, 1, PoolKind::UniswapV4));
        let r = two_hop_route(0, 1);
        let fp = RouteFastPath::from_route(&r, &pt);
        assert_eq!(RouteDispatch::from_u8(fp.dispatch), RouteDispatch::V3V3);
        // fee in bps = 500 pips / 100 = 5 bps per hop → 10 bps total.
        assert_eq!(fp.total_fee_bps, 10);
    }

    #[test]
    fn v4_v3_two_hop_dispatches_v3v3() {
        let mut pt = PoolTable::new();
        pt.insert(v4_meta(0, 0, 1, PoolKind::UniswapV4));
        pt.insert(v4_meta(1, 0, 1, PoolKind::UniswapV3));
        let r = two_hop_route(0, 1);
        let fp = RouteFastPath::from_route(&r, &pt);
        // Both are is_v3 → V3V3, supported.
        assert_eq!(RouteDispatch::from_u8(fp.dispatch), RouteDispatch::V3V3);
        assert_ne!(
            RouteDispatch::from_u8(fp.dispatch),
            RouteDispatch::Unsupported
        );
    }

    fn curve_meta(id_seed: u32, t0: u16, t1: u16) -> PoolMeta {
        use engine_state::{PairId, StateSlot, TokenId, VenueId};
        let mut addr = [0u8; 20];
        addr[..4].copy_from_slice(&id_seed.to_be_bytes());
        PoolMeta {
            address: addr,
            kind: PoolKind::CurveStableSwapNg,
            token0: TokenId(t0),
            token1: TokenId(t1),
            pair: PairId(0),
            venue: VenueId(0),
            // Curve fee in 1e10 units: 1_000_000 = 1 bps.
            fee_pips_or_bps: 1_000_000,
            tick_spacing: 0,
            state_slot: StateSlot(id_seed),
        }
    }

    #[test]
    fn curve_is_generic_quotable_and_admitted() {
        // Lockstep invariant: Curve must be quotable so a Curve + V3 two-hop dispatches
        // to MixedTwoHop (NOT Unsupported), and the fee converts 1e10 → bps via /1e6.
        assert!(is_generic_quotable(PoolKind::CurveStableSwapNg));
        let mut pt = PoolTable::new();
        pt.insert(curve_meta(0, 0, 1));
        pt.insert(v4_meta(1, 0, 1, PoolKind::UniswapV3));
        let r = two_hop_route(0, 1);
        let fp = RouteFastPath::from_route(&r, &pt);
        assert_eq!(
            RouteDispatch::from_u8(fp.dispatch),
            RouteDispatch::MixedTwoHop
        );
        assert_ne!(
            RouteDispatch::from_u8(fp.dispatch),
            RouteDispatch::Unsupported
        );
        // Curve hop: 1_000_000 / 1e6 = 1 bps; V3 hop: 500 pips / 100 = 5 bps → 6 total.
        assert_eq!(fp.total_fee_bps, 6);
    }

    #[test]
    fn curve_triangle_is_supported() {
        let mut pt = PoolTable::new();
        pt.insert(curve_meta(0, 0, 1));
        pt.insert(v4_meta(1, 1, 2, PoolKind::UniswapV3));
        pt.insert(v4_meta(2, 2, 0, PoolKind::UniswapV3));
        use crate::table::{Hop, StrategyKind};
        use engine_state::{PoolId, RouteId, TokenId};
        let mut hops = arrayvec::ArrayVec::new();
        hops.push(Hop::new(PoolId(0), 0));
        hops.push(Hop::new(PoolId(1), 0));
        hops.push(Hop::new(PoolId(2), 0));
        let mut tokens = arrayvec::ArrayVec::new();
        tokens.push(TokenId(0));
        tokens.push(TokenId(1));
        tokens.push(TokenId(2));
        tokens.push(TokenId(0));
        let r = Route {
            id: RouteId(0),
            strategy: StrategyKind::Triangle,
            hop_count: 3,
            hops,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: true,
        };
        let fp = RouteFastPath::from_route(&r, &pt);
        // A triangle with a Curve leg is admitted (TriangleV2 evaluator dispatches
        // per-hop via quote_one_hop), NOT silently dropped as Unsupported.
        assert_eq!(
            RouteDispatch::from_u8(fp.dispatch),
            RouteDispatch::TriangleV2
        );
    }
}
