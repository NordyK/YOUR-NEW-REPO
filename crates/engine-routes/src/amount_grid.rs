//! Per-route amount grid. The hot loop iterates a small (6–12) bucket set; each entry is an
//! input size in base-token units.

use alloy_primitives::U256;

/// Fixed-size amount grid. Six to twelve buckets, log-spaced between `min` and `max`.
#[derive(Debug, Clone)]
pub struct AmountGrid {
    /// Bucket inputs in base-token units, ascending.
    pub buckets: Vec<U256>,
}

impl AmountGrid {
    /// Construct a log-spaced grid with `n` buckets between `min_input` and `max_input`.
    ///
    /// Implementation walks bucket indices `i = 0..n` and computes
    /// `bucket[i] = min * (max/min)^(i/(n-1))` using integer-only operations via 64-step
    /// power-of-2 multiplication; for our purposes here we use a simple geometric series
    /// since all values are positive.
    pub fn log_spaced(min_input: U256, max_input: U256, n: usize) -> Self {
        assert!(n >= 2);
        let mut buckets = Vec::with_capacity(n);
        if min_input >= max_input {
            for _ in 0..n {
                buckets.push(min_input);
            }
            return Self { buckets };
        }
        // Compute the multiplicative factor in floating-point, then round to U256.
        let lo = min_input.bit_len() as f64;
        let hi = max_input.bit_len() as f64;
        let _ = (lo, hi);
        let n_f = (n - 1) as f64;
        // Use f64 to compute the multiplier; for very large values, geometric stepping in
        // floating point is acceptable because the grid is approximate.
        let log_lo = u256_log2(min_input);
        let log_hi = u256_log2(max_input);
        let step = (log_hi - log_lo) / n_f;
        for i in 0..n {
            let exponent = log_lo + step * (i as f64);
            buckets.push(u256_from_log2(exponent));
        }
        Self { buckets }
    }

    /// Number of buckets.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether the grid is empty.
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}

fn u256_log2(x: U256) -> f64 {
    let bits = x.bit_len();
    if bits == 0 {
        return f64::NEG_INFINITY;
    }
    // Approximate log2(x) by extracting the top 53 bits.
    let shift = bits.saturating_sub(53);
    let mantissa: U256 = if shift == 0 { x } else { x >> shift };
    let m = u256_to_f64(mantissa);
    m.log2() + (shift as f64)
}

fn u256_to_f64(x: U256) -> f64 {
    // Take low 64 bits if value fits, otherwise approximate.
    if x.bit_len() <= 53 {
        let limbs = x.as_limbs();
        return limbs[0] as f64;
    }
    let bits = x.bit_len();
    let shift = bits - 53;
    let trunc: U256 = x >> shift;
    let limbs = trunc.as_limbs();
    let m = limbs[0] as f64;
    m * (2f64).powi(shift as i32)
}

fn u256_from_log2(exp: f64) -> U256 {
    if !exp.is_finite() || exp <= 0.0 {
        return U256::from(1u64);
    }
    let floor = exp.floor() as i32;
    let frac = exp - (floor as f64);
    let frac_mantissa = (2f64).powf(frac);
    // mantissa * 2^floor; we hold mantissa as integer scaled by 2^53.
    let scaled = (frac_mantissa * (1u64 << 53) as f64) as u128;
    let mantissa_u256: U256 = U256::from(scaled);
    if floor >= 53 {
        mantissa_u256 << (floor - 53)
    } else {
        mantissa_u256 >> (53 - floor)
    }
}

/// Library of amount grids indexed by grid id.
pub struct AmountGridLibrary {
    grids: Vec<AmountGrid>,
}

impl AmountGridLibrary {
    /// Empty.
    pub fn new() -> Self {
        Self { grids: Vec::new() }
    }

    /// Insert.
    pub fn insert(&mut self, g: AmountGrid) -> u16 {
        let id = self.grids.len() as u16;
        self.grids.push(g);
        id
    }

    /// Hot-path lookup.
    #[inline]
    pub fn get(&self, id: u16) -> &AmountGrid {
        &self.grids[id as usize]
    }

    /// Count.
    pub fn len(&self) -> usize {
        self.grids.len()
    }

    /// Whether the library is empty.
    pub fn is_empty(&self) -> bool {
        self.grids.is_empty()
    }
}

impl Default for AmountGridLibrary {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_is_ascending() {
        let g = AmountGrid::log_spaced(U256::from(100u64), U256::from(1_000_000u64), 6);
        assert_eq!(g.buckets.len(), 6);
        for w in g.buckets.windows(2) {
            assert!(
                w[1] >= w[0],
                "buckets must be non-decreasing: {:?}",
                g.buckets
            );
        }
    }

    #[test]
    fn grid_bounds_respected() {
        let g = AmountGrid::log_spaced(U256::from(1u64) << 60, U256::from(1u64) << 70, 4);
        assert!(g.buckets[0] >= U256::from(1u64) << 59);
        assert!(g.buckets.last().unwrap() <= &(U256::from(1u64) << 71));
    }
}
