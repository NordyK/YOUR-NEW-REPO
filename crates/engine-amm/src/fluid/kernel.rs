//! Generic AMM kernel adapter.
//!
//! Routers usually want one enum and two methods for graph search. The enum in
//! this module wraps Fluid v1 and v2 state without adding discovery/execution
//! concerns or heap allocations.

use alloy_primitives::U256;

use super::error::Result;
use super::fluid_v1::{V1ExactOutRounding, V1PoolState};
use super::fluid_v2::{V2Boundary, V2PoolState, V2QuoteOptions};
use super::types::{ExactInRequest, ExactOutRequest, KernelExactInQuote, KernelExactOutQuote};

#[derive(Debug, Clone, Copy)]
pub enum FluidAmmPoolRef<'a> {
    V1(&'a V1PoolState),
    V2 {
        pool: &'a V2PoolState,
        /// Boundaries sorted for the exact quote direction. Keep separate hot
        /// arrays for zero-to-one and one-to-zero in the host cache.
        boundaries: &'a [V2Boundary],
        options: V2QuoteOptions,
    },
}

impl<'a> FluidAmmPoolRef<'a> {
    #[inline(always)]
    pub fn quote_exact_in(self, request: ExactInRequest) -> Result<KernelExactInQuote> {
        match self {
            Self::V1(pool) => {
                let q = pool.quote_exact_in(request.direction, request.amount_in, request.now)?;
                Ok(KernelExactInQuote {
                    amount_in: q.amount_in_native,
                    amount_out: q.amount_out_native,
                    effective_fee_paid: q.split.total_fee_adjusted()?,
                })
            }
            Self::V2 {
                pool,
                boundaries,
                options,
            } => {
                let q =
                    pool.quote_exact_in(request.direction, request.amount_in, boundaries, options)?;
                Ok(KernelExactInQuote {
                    amount_in: q.amount_in_consumed,
                    amount_out: q.amount_out,
                    effective_fee_paid: q.fee_paid,
                })
            }
        }
    }

    #[inline(always)]
    pub fn quote_exact_out(self, request: ExactOutRequest) -> Result<KernelExactOutQuote> {
        match self {
            Self::V1(pool) => {
                let q = pool.quote_exact_out(
                    request.direction,
                    request.amount_out,
                    request.now,
                    V1ExactOutRounding::ExecutionSafe,
                )?;
                Ok(KernelExactOutQuote {
                    amount_in: q.amount_in_native,
                    amount_out: q.amount_out_native,
                    effective_fee_paid: q.split.total_fee_adjusted()?,
                })
            }
            Self::V2 {
                pool,
                boundaries,
                options,
            } => {
                let q = pool.quote_exact_out(
                    request.direction,
                    request.amount_out,
                    boundaries,
                    options,
                )?;
                Ok(KernelExactOutQuote {
                    amount_in: q.amount_in,
                    amount_out: q.amount_out_satisfied,
                    effective_fee_paid: q.fee_paid,
                })
            }
        }
    }

    #[inline(always)]
    pub fn is_v1(self) -> bool {
        matches!(self, Self::V1(_))
    }

    #[inline(always)]
    pub fn is_v2(self) -> bool {
        matches!(self, Self::V2 { .. })
    }
}

/// Small helper for host-side routers that want a fast zero quote without
/// constructing a full error policy.
#[inline(always)]
pub fn zero_exact_in_quote(amount_in: U256) -> KernelExactInQuote {
    KernelExactInQuote {
        amount_in,
        amount_out: U256::ZERO,
        effective_fee_paid: U256::ZERO,
    }
}
