//! Compressed-sparse-row index for `pool_id → route_id` lookups.
//!
//! Two arrays:
//! * `offsets: [u32; N+1]` indexed by outer entity (pool id).
//! * `ids: [u32; total]` flattened ids.
//!
//! `slice_for(p)` returns `&ids[offsets[p]..offsets[p+1]]`. Two array reads, one slice.

use engine_state::{PoolId, RouteId};

/// Immutable CSR index. Built once at startup; never mutated on the hot path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Csr {
    offsets: Vec<u32>,
    ids: Vec<RouteId>,
}

impl Csr {
    /// Hot-path lookup. Returns the slice of route ids for `pool`.
    #[inline]
    pub fn slice_for(&self, pool: PoolId) -> &[RouteId] {
        let p = pool.0 as usize;
        let start = self.offsets[p] as usize;
        let end = self.offsets[p + 1] as usize;
        &self.ids[start..end]
    }

    /// Number of outer entities.
    pub fn outer_len(&self) -> usize {
        if self.offsets.is_empty() {
            0
        } else {
            self.offsets.len() - 1
        }
    }

    /// Total payload length.
    pub fn total_inner(&self) -> usize {
        self.ids.len()
    }
}

/// Builder that accumulates `(outer, inner)` pairs and finalises into a CSR.
pub struct CsrBuilder {
    pairs: Vec<(u32, u32)>,
    outer_count: u32,
}

impl CsrBuilder {
    /// New builder with a known outer-entity count.
    pub fn new(outer_count: u32) -> Self {
        Self {
            pairs: Vec::new(),
            outer_count,
        }
    }

    /// Append a `(outer, inner)` pair. Outer must be `< outer_count`.
    pub fn push(&mut self, outer: u32, inner: RouteId) {
        debug_assert!(outer < self.outer_count);
        self.pairs.push((outer, inner.0));
    }

    /// Build the final CSR (sorts by outer, deduplicates).
    pub fn build(mut self) -> Csr {
        // Sort by outer ascending, then by inner ascending for determinism.
        self.pairs.sort_unstable();
        self.pairs.dedup();
        let mut offsets = vec![0u32; (self.outer_count + 1) as usize];
        let mut ids = Vec::with_capacity(self.pairs.len());
        let mut current_outer: u32 = 0;
        for &(outer, inner) in &self.pairs {
            while current_outer < outer {
                current_outer += 1;
                offsets[current_outer as usize] = ids.len() as u32;
            }
            ids.push(RouteId(inner));
        }
        while current_outer < self.outer_count {
            current_outer += 1;
            offsets[current_outer as usize] = ids.len() as u32;
        }
        Csr { offsets, ids }
    }

    /// Build the CSR sorting routes inside each pool by `route_score` descending.
    /// This is the §6.2 "score-based pre-cull" — when sim_loop iterates
    /// `slice_for(pool_id)` and caps at the first N entries, those N are now the
    /// highest-scoring routes touching the dirty pool, not just the first N by
    /// insertion order.
    ///
    /// `route_scores` is indexed by `route_id.0 as usize` and gives a higher-is-better
    /// score. Routes with equal scores fall back to ascending `route_id` for
    /// determinism.
    pub fn build_score_ordered(mut self, route_scores: &[u32]) -> Csr {
        // Stable sort: outer ASC, then score DESC, then inner ASC.
        self.pairs.sort_by(|a, b| {
            let pool_cmp = a.0.cmp(&b.0);
            if pool_cmp != std::cmp::Ordering::Equal {
                return pool_cmp;
            }
            let sa = route_scores.get(a.1 as usize).copied().unwrap_or(0);
            let sb = route_scores.get(b.1 as usize).copied().unwrap_or(0);
            // Descending on score.
            sb.cmp(&sa).then(a.1.cmp(&b.1))
        });
        self.pairs.dedup();
        let mut offsets = vec![0u32; (self.outer_count + 1) as usize];
        let mut ids = Vec::with_capacity(self.pairs.len());
        let mut current_outer: u32 = 0;
        for &(outer, inner) in &self.pairs {
            while current_outer < outer {
                current_outer += 1;
                offsets[current_outer as usize] = ids.len() as u32;
            }
            ids.push(RouteId(inner));
        }
        while current_outer < self.outer_count {
            current_outer += 1;
            offsets[current_outer as usize] = ids.len() as u32;
        }
        Csr { offsets, ids }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csr_builds_and_slices() {
        let mut b = CsrBuilder::new(4);
        b.push(0, RouteId(10));
        b.push(0, RouteId(11));
        b.push(2, RouteId(20));
        b.push(3, RouteId(30));
        b.push(3, RouteId(31));
        let csr = b.build();
        assert_eq!(csr.slice_for(PoolId(0)), &[RouteId(10), RouteId(11)]);
        assert_eq!(csr.slice_for(PoolId(1)), &[]);
        assert_eq!(csr.slice_for(PoolId(2)), &[RouteId(20)]);
        assert_eq!(csr.slice_for(PoolId(3)), &[RouteId(30), RouteId(31)]);
        assert_eq!(csr.outer_len(), 4);
        assert_eq!(csr.total_inner(), 5);
    }

    #[test]
    fn csr_dedups() {
        let mut b = CsrBuilder::new(2);
        b.push(0, RouteId(1));
        b.push(0, RouteId(1));
        let csr = b.build();
        assert_eq!(csr.slice_for(PoolId(0)), &[RouteId(1)]);
    }

    #[test]
    fn csr_score_ordered_returns_highest_first() {
        let mut b = CsrBuilder::new(1);
        b.push(0, RouteId(10));
        b.push(0, RouteId(11));
        b.push(0, RouteId(12));
        // route_id 0..=12 → scores indexed by id; only 10/11/12 matter.
        let mut scores = vec![0u32; 13];
        scores[10] = 50;
        scores[11] = 99; // highest
        scores[12] = 1;
        let csr = b.build_score_ordered(&scores);
        // Routes inside pool 0 must be sorted by score DESC: 11 (99), 10 (50), 12 (1).
        assert_eq!(
            csr.slice_for(PoolId(0)),
            &[RouteId(11), RouteId(10), RouteId(12)]
        );
    }

    #[test]
    fn csr_score_ordered_tiebreaks_on_route_id() {
        let mut b = CsrBuilder::new(1);
        b.push(0, RouteId(5));
        b.push(0, RouteId(2));
        b.push(0, RouteId(8));
        let scores = vec![10u32; 10]; // all equal — tiebreak by id ASC
        let csr = b.build_score_ordered(&scores);
        assert_eq!(
            csr.slice_for(PoolId(0)),
            &[RouteId(2), RouteId(5), RouteId(8)]
        );
    }
}
