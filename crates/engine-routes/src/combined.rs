//! §24 self-backrun atomic Mode-A combined route builder.
//!
//! After primary route `R` clears all gates, the sim worker probes follow-up routes `F`
//! from `self_impact_index[R]` against an overlay state. The best `F` is combined with `R`
//! into a single executor call via a combined `RouteKind`.

use arrayvec::ArrayVec;

use engine_state::{RouteId, TokenId};

use crate::calldata::RouteKind;
use crate::table::{Hop, Route, StrategyKind, MAX_HOPS};

/// Combined primary + follow-up route.
#[derive(Debug, Clone)]
pub struct CombinedRoute {
    /// Combined dense id.
    pub id: RouteId,
    /// Primary route id (`R`).
    pub primary: RouteId,
    /// Follow-up route id (`F`).
    pub follow_up: RouteId,
    /// Combined route kind (§11). Phase 1 supports V3/V3 + V3/V3 and V2/V3 + V2/V3.
    pub combined_kind: RouteKind,
    /// Hop sequence of `R || F` (length = R.hop_count + F.hop_count, ≤ MAX_HOPS).
    pub hops: ArrayVec<Hop, MAX_HOPS>,
    /// Token cycle for the combined call. Length = R.hop_count + F.hop_count + 1.
    pub tokens: ArrayVec<TokenId, { MAX_HOPS + 1 }>,
}

/// Build a combined route given primary + follow-up, validating that:
/// * the follow-up's base token matches the primary's base token,
/// * the total hop count ≤ MAX_HOPS,
/// * the follow-up's combined kind is one of the §11 admitted combined kinds.
///
/// NOTE: the primary and follow-up legs INTENTIONALLY may share a pool. §24 self-backrun
/// re-trades the very pool the primary just moved (that is the whole point of the strategy),
/// so there is deliberately NO pool-disjointness check here. A combined route whose hop
/// sequence references the same `PoolId` twice is valid by design.
pub fn build_combined(
    next_id: RouteId,
    primary: &Route,
    follow_up: &Route,
) -> Result<CombinedRoute, CombineError> {
    if primary.tokens.first() != follow_up.tokens.first() {
        return Err(CombineError::BaseTokenMismatch);
    }
    let combined_hops = primary.hop_count + follow_up.hop_count;
    if combined_hops as usize > MAX_HOPS {
        return Err(CombineError::HopCountExceeded);
    }
    let kind = match (primary.strategy, follow_up.strategy) {
        (StrategyKind::TwoHop, StrategyKind::TwoHop) => {
            // Map by the family signature of the primary; V3V3 + V3V3, V2V3 + V2V3.
            if primary.family_signature == 0x33 && follow_up.family_signature == 0x33 {
                RouteKind::V3V3SelfExtendV3V3
            } else if primary.family_signature == 0x31 && follow_up.family_signature == 0x31 {
                RouteKind::V2V3SelfExtendV2V3
            } else {
                return Err(CombineError::UnsupportedCombinedKind);
            }
        }
        _ => return Err(CombineError::UnsupportedCombinedKind),
    };
    let mut hops: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
    for h in &primary.hops {
        hops.push(*h);
    }
    for h in &follow_up.hops {
        hops.push(*h);
    }
    let mut tokens: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
    for t in &primary.tokens {
        tokens.push(*t);
    }
    // Drop the last token of primary because the first token of follow-up duplicates it.
    let _ = tokens.pop();
    for t in &follow_up.tokens {
        tokens.push(*t);
    }
    Ok(CombinedRoute {
        id: next_id,
        primary: primary.id,
        follow_up: follow_up.id,
        combined_kind: kind,
        hops,
        tokens,
    })
}

/// Combine errors.
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum CombineError {
    /// Primary and follow-up start in different base tokens.
    #[error("base token mismatch between primary and follow-up")]
    BaseTokenMismatch,
    /// Combined hop count exceeds `MAX_HOPS`.
    #[error("combined hop count exceeds MAX_HOPS")]
    HopCountExceeded,
    /// Combined route kind not in the §11 admitted set.
    #[error("unsupported combined route kind")]
    UnsupportedCombinedKind,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Hop, RouteTable, StrategyKind};
    use arrayvec::ArrayVec;
    use engine_state::{PoolId, TokenId};

    fn dummy_route(id: u32, family_sig: u16) -> Route {
        let mut hops: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
        hops.push(Hop::new(PoolId(id * 2), 0));
        hops.push(Hop::new(PoolId(id * 2 + 1), 0));
        let mut tokens: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
        tokens.push(TokenId(0));
        tokens.push(TokenId(1));
        tokens.push(TokenId(0));
        Route {
            id: RouteId(id),
            strategy: StrategyKind::TwoHop,
            hop_count: 2,
            hops,
            tokens,
            family_signature: family_sig,
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
    fn combine_v3_v3() {
        let r = dummy_route(0, 0x33);
        let f = dummy_route(1, 0x33);
        let c = build_combined(RouteId(99), &r, &f).unwrap();
        assert_eq!(c.combined_kind, RouteKind::V3V3SelfExtendV3V3);
        assert_eq!(c.hops.len(), 4);
        let _ = RouteTable::new();
    }

    #[test]
    fn combine_allows_shared_pool_primary_and_follow_up() {
        // §24 self-backrun INTENTIONALLY re-trades the very pool the primary moved, so
        // `build_combined` must accept a follow-up that shares a pool with the primary.
        // There is no disjointness check by design (documented on `build_combined`).
        let r = dummy_route(0, 0x33);
        // Build a follow-up whose hops reference the SAME PoolIds as the primary (0 and 1).
        let mut f = dummy_route(1, 0x33);
        let mut shared_hops: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
        shared_hops.push(Hop::new(PoolId(0), 0)); // == r's first hop pool
        shared_hops.push(Hop::new(PoolId(1), 0)); // == r's second hop pool
        f.hops = shared_hops;

        let c = build_combined(RouteId(99), &r, &f).expect("shared-pool combine must succeed");
        assert_eq!(c.combined_kind, RouteKind::V3V3SelfExtendV3V3);
        assert_eq!(c.hops.len(), 4);
        // The combined hop sequence contains the shared pool more than once, by design.
        let shared_count = c.hops.iter().filter(|h| h.pool == PoolId(0)).count();
        assert!(
            shared_count >= 2,
            "primary+follow-up may share a pool (§24)"
        );
    }

    #[test]
    fn unsupported_kind_rejected() {
        let r = dummy_route(0, 0x00);
        let f = dummy_route(1, 0x00);
        let res = build_combined(RouteId(99), &r, &f);
        assert!(matches!(res, Err(CombineError::UnsupportedCombinedKind)));
    }
}
