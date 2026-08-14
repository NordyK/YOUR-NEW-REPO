//! Self-impact index: for each route `R`, the list of follow-up routes `F` sharing at least
//! one pool with `R` (excluding `R` itself), used by §24 self-backrun.

use ahash::AHashSet;
use engine_state::RouteId;

use crate::csr::{Csr, CsrBuilder};
use crate::table::RouteTable;

/// Wraps a CSR of route→route ids.
pub struct SelfImpactIndex {
    inner: Csr,
}

impl SelfImpactIndex {
    /// Hot-path slice for route `r`.
    #[inline]
    pub fn slice_for(&self, route: RouteId) -> &[RouteId] {
        // The CSR uses `PoolId(u32)` for keying but here the outer key is `RouteId(u32)`.
        // We reuse the same dense layout by wrapping a u32-keyed CSR directly.
        self.inner.slice_for(engine_state::PoolId(route.0))
    }
}

/// Builder.
pub struct SelfImpactBuilder<'a> {
    pool_to_hot_routes: &'a Csr,
    table: &'a RouteTable,
    top_k: usize,
}

impl<'a> SelfImpactBuilder<'a> {
    /// New builder. `pool_to_hot_routes` is the standard CSR built by `engine-detect` over
    /// hot routes. `top_k` is the maximum follow-ups per route kept (§24 default = 16).
    pub fn new(pool_to_hot_routes: &'a Csr, table: &'a RouteTable, top_k: usize) -> Self {
        Self {
            pool_to_hot_routes,
            table,
            top_k,
        }
    }

    /// Build the self-impact CSR. For every hot route `r`, walk its hops, union the
    /// pool→route slice for each touched pool, drop `r` itself, keep top-`K` by
    /// `route_heat`.
    pub fn build(self) -> SelfImpactIndex {
        let total = self.table.len();
        let mut csr = CsrBuilder::new(total as u32);
        let mut buf: AHashSet<RouteId> = AHashSet::with_capacity(64);
        for r in self.table.all() {
            if !r.hot {
                continue;
            }
            buf.clear();
            for hop in &r.hops {
                for &candidate in self.pool_to_hot_routes.slice_for(hop.pool) {
                    if candidate != r.id {
                        buf.insert(candidate);
                    }
                }
            }
            // Rank: by route_heat desc, then by id.
            let mut ranked: Vec<RouteId> = buf.iter().copied().collect();
            ranked.sort_unstable_by(|a, b| {
                let ha = self.table.get(*a).route_heat;
                let hb = self.table.get(*b).route_heat;
                hb.cmp(&ha).then_with(|| a.0.cmp(&b.0))
            });
            for &id in ranked.iter().take(self.top_k) {
                csr.push(r.id.0, id);
            }
        }
        SelfImpactIndex { inner: csr.build() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csr::CsrBuilder;
    use crate::table::{Hop, Route, StrategyKind};
    use arrayvec::ArrayVec;
    use engine_state::{PoolId, TokenId};

    fn dummy_route(id: u32, pools: &[u32]) -> Route {
        let mut hops = ArrayVec::new();
        let mut tokens = ArrayVec::new();
        tokens.push(TokenId(0));
        for &p in pools {
            hops.push(Hop::new(PoolId(p), 0));
            tokens.push(TokenId(0));
        }
        Route {
            id: RouteId(id),
            strategy: StrategyKind::TwoHop,
            hop_count: pools.len() as u8,
            hops,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 1,
            hot: true,
        }
    }

    #[test]
    fn self_impact_finds_overlap() {
        let mut t = RouteTable::new();
        t.insert(dummy_route(0, &[0, 1])); // R0 touches pools 0,1
        t.insert(dummy_route(0, &[1, 2])); // R1 touches pools 1,2 (shares pool 1 with R0)
        t.insert(dummy_route(0, &[3, 4])); // R2 disjoint

        let mut b = CsrBuilder::new(5);
        b.push(0, RouteId(0));
        b.push(1, RouteId(0));
        b.push(1, RouteId(1));
        b.push(2, RouteId(1));
        b.push(3, RouteId(2));
        b.push(4, RouteId(2));
        let pool_to_route = b.build();

        let si = SelfImpactBuilder::new(&pool_to_route, &t, 16).build();
        let s0 = si.slice_for(RouteId(0));
        let s1 = si.slice_for(RouteId(1));
        let s2 = si.slice_for(RouteId(2));
        assert!(s0.contains(&RouteId(1)) && !s0.contains(&RouteId(0)) && !s0.contains(&RouteId(2)));
        assert!(s1.contains(&RouteId(0)) && !s1.contains(&RouteId(1)) && !s1.contains(&RouteId(2)));
        assert!(s2.is_empty());
    }
}
