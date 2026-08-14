//! Route table data structures.

use arrayvec::ArrayVec;

use engine_state::{PoolId, RouteId, TokenId};

/// Maximum supported hops per route (§5.3 raises to 8).
pub const MAX_HOPS: usize = 8;

/// Route strategy classification.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StrategyKind {
    /// Two-hop cycle.
    TwoHop = 2,
    /// Three-hop triangle.
    Triangle = 3,
    /// Four-hop.
    FourHop = 4,
    /// Five-hop.
    FiveHop = 5,
    /// Six-hop.
    SixHop = 6,
    /// Seven-hop.
    SevenHop = 7,
    /// Eight-hop.
    EightHop = 8,
    /// Self-extension follow-up (§24).
    SelfImpact = 255,
}

/// Packed hop record. 8 bytes per hop = 64 bytes per 8-hop route (one cache line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hop {
    /// Pool id.
    pub pool: PoolId,
    /// Direction (0 = token0→token1, 1 = token1→token0).
    pub direction: u8,
    /// Fee-tier index for V3-family (0..=N), or 0 for non-V3.
    pub fee_tier_id: u8,
    /// Padding (kept explicit for layout stability).
    pub _pad: u16,
}

impl Hop {
    /// New hop with default fee tier 0 and padding 0.
    pub const fn new(pool: PoolId, direction: u8) -> Self {
        Self {
            pool,
            direction,
            fee_tier_id: 0,
            _pad: 0,
        }
    }
}

/// Full route record. The hot path reads the strategy kind, hops, and template ids.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Route {
    /// Dense id.
    pub id: RouteId,
    /// Strategy.
    pub strategy: StrategyKind,
    /// Hop count (2..=8).
    pub hop_count: u8,
    /// Hops in cycle order.
    pub hops: ArrayVec<Hop, MAX_HOPS>,
    /// Tokens visited (length = hop_count+1; start == end).
    pub tokens: ArrayVec<TokenId, { MAX_HOPS + 1 }>,
    /// Family signature (encodes per-hop AMM family for executor dispatch).
    pub family_signature: u16,
    /// Calldata template id (`RouteKind` selector).
    pub calldata_template_id: u32,
    /// Gas template id.
    pub gas_template_id: u16,
    /// L1 size bucket template id.
    pub l1_size_bucket_template: u16,
    /// Amount-grid id.
    pub amount_grid_id: u16,
    /// Risk flag bitset.
    pub risk_flags: u64,
    /// Readiness mask bitset.
    pub readiness_mask: u64,
    /// Route heat score.
    pub route_heat: u32,
    /// Whether this route is hot-eligible.
    pub hot: bool,
}

/// Dense route table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RouteTable {
    routes: Vec<Route>,
    hot_route_ids: Vec<RouteId>,
}

impl RouteTable {
    /// Empty.
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            hot_route_ids: Vec::new(),
        }
    }

    /// Insert and assign a dense id.
    pub fn insert(&mut self, mut route: Route) -> RouteId {
        let id = RouteId(self.routes.len() as u32);
        route.id = id;
        if route.hot {
            self.hot_route_ids.push(id);
        }
        self.routes.push(route);
        id
    }

    /// Get route by id.
    pub fn get(&self, id: RouteId) -> &Route {
        &self.routes[id.0 as usize]
    }

    /// Mutable get (control-plane only).
    pub fn get_mut(&mut self, id: RouteId) -> &mut Route {
        &mut self.routes[id.0 as usize]
    }

    /// B4: hint the kernel that the contiguous routes Vec should be backed by 2 MB huge
    /// pages. For ~50K routes × ~128 B/Route = ~6 MiB this collapses 1500 TLB entries → 3,
    /// a real win on the hot marginal-product scan over `seen` routes. Best-effort —
    /// `false` on non-Linux or when the kernel/cgroup denies the advice.
    pub fn madvise_hugepage(&self) -> bool {
        if self.routes.capacity() == 0 {
            return false;
        }
        let ptr = self.routes.as_ptr() as *const u8;
        let len = self.routes.capacity() * std::mem::size_of::<Route>();
        engine_sysopt::madvise_hugepage_region(ptr, len)
    }

    /// All routes.
    pub fn all(&self) -> &[Route] {
        &self.routes
    }

    /// All hot route ids.
    pub fn hot_ids(&self) -> &[RouteId] {
        &self.hot_route_ids
    }

    /// Number of routes.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Whether the route table is empty.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

impl Default for RouteTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_route(hops: u8, hot: bool) -> Route {
        let mut hops_arr = ArrayVec::new();
        let mut tokens = ArrayVec::new();
        tokens.push(TokenId(0));
        for i in 0..hops {
            hops_arr.push(Hop::new(PoolId(i as u32), 0));
            tokens.push(TokenId(i as u16 + 1));
        }
        // Close the cycle.
        *tokens.last_mut().unwrap() = TokenId(0);
        Route {
            id: RouteId(0),
            strategy: match hops {
                2 => StrategyKind::TwoHop,
                3 => StrategyKind::Triangle,
                _ => StrategyKind::FourHop,
            },
            hop_count: hops,
            hops: hops_arr,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot,
        }
    }

    #[test]
    fn insert_and_collect_hot() {
        let mut t = RouteTable::new();
        let r1 = t.insert(dummy_route(2, true));
        let _r2 = t.insert(dummy_route(2, false));
        let r3 = t.insert(dummy_route(3, true));
        assert_eq!(t.len(), 3);
        assert_eq!(t.hot_ids(), &[r1, r3]);
    }
}
