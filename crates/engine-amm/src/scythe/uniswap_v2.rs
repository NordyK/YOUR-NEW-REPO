//! Uniswap V2 fast-path pricing + live state tracking (alloy.rs)
//!
//! Goals
//! - No async/RPC in hot-path (relay logs update state)
//! - Exact Uniswap V2 math (fee generalized)
//! - Works with sequencer/relay logs (topics + data)
//! - Alloy primitives only; zero-copy decoding; inline-heavy

use alloy_primitives::{Address, U256};

// ============================================================================
// FEE FACTOR CACHE (QW-004 optimization)
// ============================================================================
// Pre-computed fee factors for common V2/Solidly fees (basis points).
// Avoids repeated `10000 - fee_bps` calculation in hot path.
// Called 60-180× per quote during binary search.

// ============================================================================
// FeeBps — canonical fee representation (basis points)
// ============================================================================

/// Canonical V2/Solidly fee representation in basis points.
///
/// Unifies the old dual representation (V2Fee{num,den} vs raw fee_bps)
/// into a single newtype. 30 bps = 0.30% fee (UniV2 default).
///
/// # Math equivalence
///
/// `FeeBps(30)` produces factor=9970, which is algebraically identical
/// to the old `V2Fee { num: 997, den: 1000 }`:
/// `amount_in * 9970 / (reserve_in * 10000 + amount_in * 9970)`
/// = `amount_in * 997 / (reserve_in * 1000 + amount_in * 997)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeBps(u16);

const FEE_FACTOR_30BPS: u16 = 9970; // 10000 - 30 (UniV2 default 0.30%)
const FEE_FACTOR_25BPS: u16 = 9975; // 10000 - 25 (0.25%)
const FEE_FACTOR_5BPS: u16 = 9995; // 10000 - 5  (0.05%)
const FEE_FACTOR_1BPS: u16 = 9999; // 10000 - 1  (0.01%)

impl FeeBps {
    /// Create from basis points (e.g., 30 for 0.30%)
    #[inline(always)]
    pub const fn new(bps: u16) -> Self {
        Self(bps)
    }

    /// Get fee in basis points
    #[inline(always)]
    pub const fn bps(&self) -> u16 {
        self.0
    }

    /// Fee factor as raw u16 for u128 fast-path arithmetic.
    #[inline(always)]
    pub const fn factor_raw(&self) -> u16 {
        match self.0 {
            30 => FEE_FACTOR_30BPS,
            25 => FEE_FACTOR_25BPS,
            5 => FEE_FACTOR_5BPS,
            1 => FEE_FACTOR_1BPS,
            _ => 10000u16.saturating_sub(self.0),
        }
    }

    /// Get fee factor as U256: `10000 - fee_bps`.
    /// Common fees use compile-time constants; rare fees compute at runtime.
    #[inline(always)]
    pub fn factor(&self) -> U256 {
        U256::from(self.factor_raw())
    }
}

impl Default for FeeBps {
    /// Default UniV2 fee: 30 bps (0.30%)
    #[inline(always)]
    fn default() -> Self {
        Self(30)
    }
}

/// Get cached fee factor for V2/Solidly swaps.
/// Convenience wrapper around `FeeBps::factor()`.
#[inline(always)]
pub fn get_fee_factor_v2(fee_bps: u16) -> U256 {
    FeeBps::new(fee_bps).factor()
}

/// Lightweight V2 pair state kept in memory and updated by logs
#[derive(Debug, Clone)]
pub struct V2Pair {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee: FeeBps,
    pub last_block: u64,
    pub version: u64, // monotonically increasing state version
}

impl V2Pair {
    #[inline]
    pub fn new(
        address: Address,
        token0: Address,
        token1: Address,
        reserve0: U256,
        reserve1: U256,
    ) -> Self {
        Self {
            address,
            token0,
            token1,
            reserve0,
            reserve1,
            fee: FeeBps::default(),
            last_block: 0,
            version: 0,
        }
    }

    /// Seed from an RPC snapshot (reserves, tokens)
    #[inline]
    pub fn seed_from_snapshot(
        &mut self,
        token0: Address,
        token1: Address,
        reserve0: U256,
        reserve1: U256,
        block: u64,
    ) {
        self.token0 = token0;
        self.token1 = token1;
        self.reserve0 = reserve0;
        self.reserve1 = reserve1;
        self.last_block = block;
        self.version = self.version.wrapping_add(1);
    }

    /// Quote exact-in swap. Delegates to `get_amount_out()` (u128 fast path + U256 fallback).
    #[inline]
    pub fn quote_exact_in(
        &self,
        amount_in: U256,
        zero_for_one: bool,
    ) -> crate::scythe::errors::V2MathResult<U256> {
        let (reserve_in, reserve_out) = if zero_for_one {
            (self.reserve0, self.reserve1)
        } else {
            (self.reserve1, self.reserve0)
        };

        if amount_in.is_zero() {
            return Ok(U256::ZERO);
        }
        // A zero reserve is a degenerate/uninitialised pool — fail closed (Err) rather than
        // returning Ok(0), which a caller would read as a legitimate "no profit" quote. Mirrors
        // `quote_exact_out` and the authoritative `engine-amm/src/uniswap_v2.rs`.
        if reserve_in.is_zero() || reserve_out.is_zero() {
            return Err(crate::scythe::errors::V2MathError::ZeroLiquidity);
        }

        get_amount_out(self.fee.bps(), amount_in, reserve_in, reserve_out)
    }

    /// Quote amountIn required for exact-out swap. zero_for_one = buy token1 with token0.
    #[inline]
    pub fn quote_exact_out(
        &self,
        amount_out: U256,
        zero_for_one: bool,
    ) -> crate::scythe::errors::V2MathResult<U256> {
        let (reserve_in, reserve_out) = if zero_for_one {
            (self.reserve0, self.reserve1)
        } else {
            (self.reserve1, self.reserve0)
        };

        if amount_out.is_zero() {
            return Ok(U256::ZERO);
        }
        if amount_out >= reserve_out {
            return Err(crate::scythe::errors::V2MathError::ZeroLiquidity);
        }

        let fee_factor = self.fee.factor();
        let ten_thousand = U256::from(10000u32);
        // amountIn = ceil( reserve_in * amount_out * 10000 / ( (reserve_out - amount_out) * fee_factor) )
        let r_out_minus = reserve_out - amount_out;
        if r_out_minus.is_zero() {
            return Err(crate::scythe::errors::V2MathError::ArithmeticOverflow);
        }

        let numerator = reserve_in
            .checked_mul(amount_out)
            .ok_or(crate::scythe::errors::V2MathError::ArithmeticOverflow)?
            .checked_mul(ten_thousand)
            .ok_or(crate::scythe::errors::V2MathError::ArithmeticOverflow)?;
        let denominator = r_out_minus
            .checked_mul(fee_factor)
            .ok_or(crate::scythe::errors::V2MathError::ArithmeticOverflow)?;

        if denominator.is_zero() {
            return Err(crate::scythe::errors::V2MathError::ArithmeticOverflow);
        }

        // Match Solidity getAmountIn: always adds 1 (even when division is exact)
        let amount_in = (numerator / denominator) + U256::from(1u8);
        Ok(amount_in)
    }

    /// Mid-price as f64 (token0 per token1 or vice versa). For human display only.
    #[inline]
    pub fn mid_price_0_over_1(&self) -> f64 {
        // price of token0 in token1 (token1/token0?)
        // define as token1 per token0 = reserve1 / reserve0
        if self.reserve0.is_zero() {
            return f64::INFINITY;
        }
        let r0 = self.reserve0.to::<u128>() as f64;
        let r1 = self.reserve1.to::<u128>() as f64;
        r1 / r0
    }

    #[inline]
    pub fn mid_price_1_over_0(&self) -> f64 {
        if self.reserve1.is_zero() {
            return f64::INFINITY;
        }
        let r0 = self.reserve0.to::<u128>() as f64;
        let r1 = self.reserve1.to::<u128>() as f64;
        r0 / r1
    }
}

/// Manager for many pairs; tiny cache for hot path pricing
#[derive(Default)]
pub struct V2Book {
    pub pairs: ahash::AHashMap<Address, V2Pair>,
}

impl V2Book {
    pub fn upsert_pair(&mut self, pair: V2Pair) {
        self.pairs.insert(pair.address, pair);
    }

    /// Fast quote for a path of V2 pairs, exact-in. `path` is [token_in, mid..., token_out].
    /// `pairs_in_order` are the pair addresses in hop order.
    pub fn quote_path_exact_in(
        &self,
        amount_in: U256,
        pairs_in_order: &[Address],
        path: &[Address],
    ) -> crate::scythe::errors::V2MathResult<U256> {
        if pairs_in_order.is_empty() || path.len() != pairs_in_order.len() + 1 {
            return Err(crate::scythe::errors::V2MathError::ZeroLiquidity);
        }
        let mut amount = amount_in;
        for (i, pair_addr) in pairs_in_order.iter().enumerate() {
            let pair = self
                .pairs
                .get(pair_addr)
                .ok_or(crate::scythe::errors::V2MathError::ZeroLiquidity)?;
            let a = path[i];
            let b = path[i + 1];
            // Determine zero_for_one relative to pair ordering
            let zero_for_one = if a == pair.token0 && b == pair.token1 {
                true
            } else if a == pair.token1 && b == pair.token0 {
                false
            } else {
                return Err(crate::scythe::errors::V2MathError::ZeroLiquidity); // path-token mismatch
            };
            // quote_exact_in now returns Result
            amount = pair.quote_exact_in(amount, zero_for_one)?;
        }
        Ok(amount)
    }
}

/// u128 fast path for V2 amount_out.
/// Returns None if any intermediate overflows u128 (caller falls back to U256).
#[inline(always)]
fn get_amount_out_u128(
    fee_factor: u128,
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
) -> Option<U256> {
    // amount_in_with_fee = amount_in * fee_factor
    let amount_in_with_fee = amount_in.checked_mul(fee_factor)?;

    // denominator = reserve_in * 10000 + amount_in_with_fee
    let reserve_in_scaled = reserve_in.checked_mul(10_000)?;
    let denominator = reserve_in_scaled.checked_add(amount_in_with_fee)?;

    if denominator == 0 {
        return None;
    }

    // numerator = amount_in_with_fee * reserve_out
    // Try pure u128 first; if overflow, use U256 for just this step
    match amount_in_with_fee.checked_mul(reserve_out) {
        Some(numerator) => Some(U256::from(numerator / denominator)),
        None => {
            // Only the numerator overflows — compute with U256 for this single mul+div
            let num = U256::from(amount_in_with_fee) * U256::from(reserve_out);
            Some(num / U256::from(denominator))
        }
    }
}

/// Standalone helper function for V2 amount_out calculation (U256 version)
///
/// Used by price_graph.rs for path simulation.
/// Uses u128 fast path when inputs fit, with U256 fallback for overflow safety.
pub fn get_amount_out(
    fee_bps: u16,
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
) -> crate::scythe::errors::V2MathResult<U256> {
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(crate::scythe::errors::V2MathError::ZeroLiquidity);
    }

    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }

    if fee_bps >= 10000 {
        // A 100% fee (factor 10000 - 10000 = 0) yields zero output and is not a valid pool fee —
        // reject it, matching the authoritative `engine-amm/src/uniswap_v2.rs` (`>= 10_000`).
        return Err(crate::scythe::errors::V2MathError::InvalidFee { fee_bps });
    }

    // --- u128 fast path ---
    let fee_factor_raw = FeeBps::new(fee_bps).factor_raw() as u128;
    if let (Ok(a), Ok(ri), Ok(ro)) = (
        TryInto::<u128>::try_into(amount_in),
        TryInto::<u128>::try_into(reserve_in),
        TryInto::<u128>::try_into(reserve_out),
    ) {
        if let Some(result) = get_amount_out_u128(fee_factor_raw, a, ri, ro) {
            return Ok(result);
        }
    }

    // --- U256 fallback (for values exceeding u128 or u128 intermediate overflow) ---
    let fee_factor = U256::from(fee_factor_raw);
    let ten_thousand = U256::from(10000u32);

    // SEC-063: Use debug level to reduce log noise in hot path (errors propagated via Result)
    let amount_in_with_fee = amount_in.checked_mul(fee_factor).ok_or_else(|| {
        log::debug!(
            "V2: Overflow in amount_in * fee_factor (amount_in={}, fee_factor={})",
            amount_in,
            fee_factor
        );
        crate::scythe::errors::V2MathError::ArithmeticOverflow
    })?;

    let numerator = amount_in_with_fee.checked_mul(reserve_out).ok_or_else(|| {
        log::debug!(
            "V2: Overflow in amount_in_with_fee * reserve_out (amount_in_with_fee={}, reserve_out={})",
            amount_in_with_fee,
            reserve_out
        );
        crate::scythe::errors::V2MathError::ArithmeticOverflow
    })?;

    let reserve_in_scaled = reserve_in.checked_mul(ten_thousand).ok_or_else(|| {
        log::debug!(
            "V2: Overflow in reserve_in * 10000 (reserve_in={})",
            reserve_in
        );
        crate::scythe::errors::V2MathError::ArithmeticOverflow
    })?;

    let denominator = reserve_in_scaled
        .checked_add(amount_in_with_fee)
        .ok_or_else(|| {
            log::debug!("V2: Overflow in reserve_in_scaled + amount_in_with_fee");
            crate::scythe::errors::V2MathError::ArithmeticOverflow
        })?;

    if denominator.is_zero() {
        return Err(crate::scythe::errors::V2MathError::ArithmeticOverflow);
    }

    Ok(numerator / denominator)
}

/// Compatibility type for price.rs (matches old ethers-based implementation)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniswapV2Reserves {
    pub reserve_0: U256,
    pub reserve_1: U256,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scythe::errors::V2MathError;

    /// A 100% fee (fee_bps == 10000) leaves zero output and must FAIL, not pass. The guard was
    /// `fee_bps > 10000`, which let exactly 10000 through (factor 10000 - 10000 = 0). Mirror the
    /// authoritative `engine-amm/src/uniswap_v2.rs`, which rejects `fee_bps >= 10_000`.
    #[test]
    fn get_amount_out_rejects_100pct_fee() {
        // Signature: get_amount_out(fee_bps, amount_in, reserve_in, reserve_out).
        let amt = U256::from(1_000_000u64);
        let res = U256::from(1_000_000u64);
        let r = get_amount_out(10_000, amt, res, res);
        assert!(
            matches!(r, Err(V2MathError::InvalidFee { fee_bps: 10000 })),
            "fee_bps == 10000 (100%) must be Err(InvalidFee), got {r:?}"
        );
        // One below the cap still computes.
        assert!(get_amount_out(9_999, amt, res, res).is_ok());
    }

    /// A zero-reserve quote is a degenerate / uninitialised pool. Returning `Ok(0)` looks like a
    /// legitimate "no profit" quote to callers; it must instead surface `Err(ZeroLiquidity)` so the
    /// path is rejected fail-closed (matching `quote_exact_out` and the authoritative kernel).
    #[test]
    fn quote_exact_in_zero_reserve_is_err() {
        let pair = V2Pair::new(
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            U256::ZERO, // reserve0 = 0
            U256::from(1_000_000u64),
        );
        // zero_for_one => reserve_in = reserve0 = 0.
        let r = pair.quote_exact_in(U256::from(1_000u64), true);
        assert!(
            matches!(r, Err(V2MathError::ZeroLiquidity)),
            "zero reserve_in must be Err(ZeroLiquidity), got {r:?}"
        );
        // The other direction has reserve_out = reserve0 = 0 — also degenerate.
        let r2 = pair.quote_exact_in(U256::from(1_000u64), false);
        assert!(
            matches!(r2, Err(V2MathError::ZeroLiquidity)),
            "zero reserve_out must be Err(ZeroLiquidity), got {r2:?}"
        );
    }
}
