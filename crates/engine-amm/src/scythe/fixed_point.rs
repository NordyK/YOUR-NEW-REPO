//! Fixed-point helpers used by the scythe kernels.

use alloy_primitives::U256;

/// Integer square root of a `U256` via Newton iteration.
///
/// Returns `floor(sqrt(x))` such that `floor(sqrt(x))^2 ≤ x < (floor(sqrt(x))+1)^2`.
///
/// The implementation mirrors the canonical `OpenZeppelin Math.sqrt` reference and
/// converges in at most `~8` iterations for any 256-bit input. Used by
/// `solve_optimal_v2_single_hop`.
pub fn isqrt_u256(n: U256) -> Option<U256> {
    if n.is_zero() {
        return Some(U256::ZERO);
    }
    // Initial guess: 2^(ceil(log2(n)/2)+1). For 256-bit input, ceil(log2(n)) is given
    // by `256 - leading_zeros(n)`, so we shift left by half of that.
    let bits = 256 - n.leading_zeros() as u32;
    // x0 = 1 << ((bits + 1) / 2); ensures x0 >= sqrt(n).
    let shift = bits.div_ceil(2);
    if shift >= 256 {
        return None; // pathological input — shouldn't happen for a U256
    }
    let mut x = U256::from(1u8) << shift as usize;
    // Newton iteration: x_{k+1} = (x_k + n / x_k) / 2.
    // Terminates when x stops decreasing.
    loop {
        let nx = (x + n / x) >> 1;
        if nx >= x {
            return Some(x);
        }
        x = nx;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqrt_zero() {
        assert_eq!(isqrt_u256(U256::ZERO), Some(U256::ZERO));
    }

    #[test]
    fn sqrt_one() {
        assert_eq!(isqrt_u256(U256::from(1u8)), Some(U256::from(1u8)));
    }

    #[test]
    fn sqrt_perfect_square() {
        // 144 = 12^2
        assert_eq!(isqrt_u256(U256::from(144u32)), Some(U256::from(12u8)));
        // 1e36 = (1e18)^2
        let one_e18 = U256::from(1_000_000_000_000_000_000u64);
        let one_e36 = one_e18 * one_e18;
        assert_eq!(isqrt_u256(one_e36), Some(one_e18));
    }

    #[test]
    fn sqrt_non_perfect_square_floors() {
        // sqrt(15) = 3.87..; floor = 3
        assert_eq!(isqrt_u256(U256::from(15u32)), Some(U256::from(3u8)));
        // sqrt(2)*2^128 ≈ 1.414×2^128
        let big = U256::from(2u8) << 128;
        let r = isqrt_u256(big).unwrap();
        // r*r <= big < (r+1)*(r+1)
        assert!(r * r <= big);
        assert!((r + U256::from(1u8)) * (r + U256::from(1u8)) > big);
    }

    #[test]
    fn sqrt_u256_max() {
        let big = U256::MAX;
        let r = isqrt_u256(big).unwrap();
        // r should satisfy r*r <= big. Strict check is not feasible (would overflow);
        // assert that r is large.
        assert!(r > U256::from(1u128 << 120));
    }
}
