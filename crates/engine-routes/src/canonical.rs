//! Route canonicalisation per §5.3 step 4.
//!
//! A route is identified as a **directed cycle**. Two enumerations collapse to a single id
//! only when they describe the **same rotation** of the same directed cycle. Reverse
//! direction is a different route. Same pools in a different order is a different route.
//! Different per-hop fee tier is a different route.

use arrayvec::ArrayVec;

use engine_state::TokenId;

use crate::table::{Hop, MAX_HOPS};

/// Canonical key for a directed cycle route. Two routes have the same key iff they are
/// rotations of each other (same hop sequence read from a different starting position).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteCanonKey {
    /// `(pool_id, direction, fee_tier_id, token_in, token_out)` per hop, normalised to start
    /// at the lexicographically smallest rotation.
    pub hops_packed: ArrayVec<(u32, u8, u8, u16, u16), MAX_HOPS>,
}

/// Compute the canonical key for a route given its hops and tokens in cycle order.
///
/// `tokens.len() == hops.len() + 1` and `tokens[0] == tokens.last()` (closed cycle).
pub fn canonicalise_route(hops: &[Hop], tokens: &[TokenId]) -> RouteCanonKey {
    debug_assert_eq!(tokens.len(), hops.len() + 1);
    debug_assert_eq!(tokens.first(), tokens.last());
    let n = hops.len();
    // Build all rotations and pick the lexicographically smallest one.
    let mut best: Option<ArrayVec<(u32, u8, u8, u16, u16), MAX_HOPS>> = None;
    for start in 0..n {
        let mut packed: ArrayVec<(u32, u8, u8, u16, u16), MAX_HOPS> = ArrayVec::new();
        for i in 0..n {
            let idx = (start + i) % n;
            let h = hops[idx];
            // token_in / token_out come from the rotated token sequence.
            let tin = tokens[idx];
            let tout = tokens[idx + 1];
            packed.push((h.pool.0, h.direction, h.fee_tier_id, tin.0, tout.0));
        }
        match &best {
            None => best = Some(packed),
            Some(cur) => {
                if packed < *cur {
                    best = Some(packed);
                }
            }
        }
    }
    RouteCanonKey {
        hops_packed: best.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrayvec::ArrayVec;
    use engine_state::PoolId;

    fn make_hops(
        pool_ids: &[u32],
    ) -> (ArrayVec<Hop, MAX_HOPS>, ArrayVec<TokenId, { MAX_HOPS + 1 }>) {
        let mut hops: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
        let mut tokens: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
        tokens.push(TokenId(0));
        for (i, &p) in pool_ids.iter().enumerate() {
            hops.push(Hop::new(PoolId(p), 0));
            tokens.push(TokenId(((i + 1) % pool_ids.len()) as u16));
        }
        (hops, tokens)
    }

    #[test]
    fn rotations_collapse_to_same_key() {
        let (h_a, t_a) = make_hops(&[1, 2, 3]);
        // Rotate.
        let (h_b, t_b) = make_hops(&[2, 3, 1]);
        let k_a = canonicalise_route(&h_a, &t_a);
        // For rotation correctness, the rotated tokens need to be the same cyclic sequence.
        // Manually compose t_b to match.
        let _ = (h_b, t_b);
        let mut hops2: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
        let mut tokens2: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
        tokens2.push(TokenId(1));
        hops2.push(Hop::new(PoolId(2), 0));
        tokens2.push(TokenId(2));
        hops2.push(Hop::new(PoolId(3), 0));
        tokens2.push(TokenId(0));
        hops2.push(Hop::new(PoolId(1), 0));
        tokens2.push(TokenId(1));
        let k_b = canonicalise_route(&hops2, &tokens2);
        assert_eq!(k_a, k_b);
    }

    #[test]
    fn reverse_direction_is_different() {
        let mut hops_a: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
        hops_a.push(Hop::new(PoolId(1), 0));
        hops_a.push(Hop::new(PoolId(2), 0));
        let mut tokens_a: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
        tokens_a.push(TokenId(0));
        tokens_a.push(TokenId(1));
        tokens_a.push(TokenId(0));

        let mut hops_b: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
        hops_b.push(Hop::new(PoolId(1), 1));
        hops_b.push(Hop::new(PoolId(2), 1));
        let k_a = canonicalise_route(&hops_a, &tokens_a);
        let k_b = canonicalise_route(&hops_b, &tokens_a);
        assert_ne!(k_a, k_b);
    }
}
