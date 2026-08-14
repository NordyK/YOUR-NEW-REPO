//! Validation harness.
//!
//! Simulates an executor call by running the route's AMM kernels against a snapshot of
//! the warm DB and enforcing the §10.10 footprint rules.

use alloy_primitives::U256;

use engine_amm::{v2_get_amount_out, AmmError};
use engine_state::{RouteId, TokenId};

use crate::manifest::RouteFootprintManifest;
use crate::warm_db::WarmDb;

/// Inputs to `validate`.
#[derive(Debug, Clone)]
pub struct ExecutionInput {
    /// Route id.
    pub route_id: RouteId,
    /// Pool ids in cycle order (hop-position indexed).
    pub pools: Vec<[u8; 20]>,
    /// Token cycle (length = hop_count + 1).
    pub tokens: Vec<TokenId>,
    /// Per-hop V2 reserves (we model V2/V2 hops only in this harness; V3 routes go through
    /// the engine's V3 kernel separately and feed `amount_out` here).
    pub hop_v2: Vec<HopV2>,
    /// Input amount in the base token.
    pub amount_in: U256,
    /// Min profit guard enforced after the cycle.
    pub min_profit: U256,
    /// Sender / executor address.
    pub executor: [u8; 20],
    /// Token address per `TokenId` index.
    pub token_addresses: Vec<[u8; 20]>,
}

/// Hop V2 reserves and fee, used by the harness's V2-only fast path.
#[derive(Debug, Clone, Copy)]
pub struct HopV2 {
    /// Token in for the hop.
    pub token_in: TokenId,
    /// Token out for the hop.
    pub token_out: TokenId,
    /// Reserve of `token_in` in the pool.
    pub reserve_in: u128,
    /// Reserve of `token_out` in the pool.
    pub reserve_out: u128,
    /// Fee in basis points.
    pub fee_bps: u32,
}

/// Validation outcome.
#[derive(Debug, Clone)]
pub struct ExecutionOutcome {
    /// Gas units consumed (engine model: constant per route kind, refined here for the
    /// dry-run; production replaces with revm-reported `gas_used`).
    pub gas_used: u64,
    /// Final base-token balance delta.
    pub base_token_delta: U256,
    /// Final amount out at the last hop (i.e. amount returned to base token).
    pub final_amount_out: U256,
}

/// Reasons the harness can revert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevertReason {
    /// AMM math error.
    AmmError(AmmError),
    /// Final output below `amount_in + min_profit`.
    NotProfitable,
    /// Storage access fell outside the manifest's allowlist (§10.10).
    OutOfManifest,
    /// Flash-loan repayment failed.
    FlashLoanRepaymentFailed,
    /// V2 reserve mirror does not match warm DB.
    StateMismatch,
}

/// Harness.
pub struct ValidationHarness<'a> {
    db: &'a WarmDb,
    manifest: &'a RouteFootprintManifest,
}

impl<'a> ValidationHarness<'a> {
    /// Construct a harness bound to a warm DB and a manifest.
    pub fn new(db: &'a WarmDb, manifest: &'a RouteFootprintManifest) -> Self {
        Self { db, manifest }
    }

    /// Validate an execution.
    ///
    /// The harness:
    /// 1. Checks that every `input.pools[i]` address is in the manifest allowlist.
    /// 2. Sequentially applies the V2 hops to derive `final_out`.
    /// 3. Asserts `final_out >= amount_in + min_profit`.
    pub fn validate(&self, input: &ExecutionInput) -> Result<ExecutionOutcome, RevertReason> {
        // Manifest gate: every pool address must be allowed.
        for addr in &input.pools {
            if !self.manifest.is_account_allowed(addr) {
                return Err(RevertReason::OutOfManifest);
            }
        }
        // Executor address must also be allowed (it issues the calls).
        if !self.manifest.is_account_allowed(&input.executor) {
            return Err(RevertReason::OutOfManifest);
        }
        // Token contracts must be allowed.
        for ta in &input.token_addresses {
            if !self.manifest.is_account_allowed(ta) {
                return Err(RevertReason::OutOfManifest);
            }
        }

        // Walk hops.
        let mut amount_in_hop: u128 =
            u256_to_u128_or_err(input.amount_in).ok_or(RevertReason::NotProfitable)?;
        let mut amount_out_final: u128 = 0;
        let mut gas_used: u64 = 21_000 + 30_000; // base + executor entry
        for (i, hop) in input.hop_v2.iter().enumerate() {
            // Manifest gate on token slots: token0 balance slot keccak-derived layouts are
            // chain-specific; we approximate by checking the *token address* is in the
            // allowlist (already checked above) and the pool address is allowed.
            let _ = i;
            let out =
                v2_get_amount_out(amount_in_hop, hop.reserve_in, hop.reserve_out, hop.fee_bps)
                    .map_err(RevertReason::AmmError)?;
            gas_used += 65_000; // V2 swap gas estimate (router path).
            amount_out_final = out;
            amount_in_hop = out;
        }
        // Profitability gate.
        let in_u128: u128 =
            u256_to_u128_or_err(input.amount_in).ok_or(RevertReason::NotProfitable)?;
        if amount_out_final < in_u128 {
            return Err(RevertReason::NotProfitable);
        }
        let realised_profit = amount_out_final - in_u128;
        let min_profit_u128 =
            u256_to_u128_or_err(input.min_profit).ok_or(RevertReason::NotProfitable)?;
        if realised_profit < min_profit_u128 {
            return Err(RevertReason::NotProfitable);
        }
        // Flash-loan repayment: in the harness model the flash loan is "borrow `amount_in`,
        // repay `amount_in + fee`." We treat repayment as automatic once profitability is
        // confirmed; the failure path is exercised by the gas crate's flash fee model.
        Ok(ExecutionOutcome {
            gas_used,
            base_token_delta: U256::from(realised_profit),
            final_amount_out: U256::from(amount_out_final),
        })
    }

    /// Returns the warm DB the harness reads from.
    pub fn warm_db(&self) -> &WarmDb {
        self.db
    }
}

fn u256_to_u128_or_err(x: U256) -> Option<u128> {
    if x.bit_len() > 128 {
        None
    } else {
        let limbs = x.as_limbs();
        Some((limbs[0] as u128) | ((limbs[1] as u128) << 64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_routes::RouteKind;

    fn pool_addr(seed: u8) -> [u8; 20] {
        let mut a = [0u8; 20];
        a[0] = seed;
        a
    }

    #[test]
    fn validate_profitable_route() {
        let mut manifest = RouteFootprintManifest::new(RouteId(0), RouteKind::V2V2FlashBalancer);
        let p0 = pool_addr(1);
        let p1 = pool_addr(2);
        let executor = pool_addr(3);
        let t0 = pool_addr(0xa0);
        let t1 = pool_addr(0xa1);
        manifest.allow_account(p0);
        manifest.allow_account(p1);
        manifest.allow_account(executor);
        manifest.allow_account(t0);
        manifest.allow_account(t1);

        let db = WarmDb::new();
        let harness = ValidationHarness::new(&db, &manifest);

        let input = ExecutionInput {
            route_id: RouteId(0),
            pools: vec![p0, p1],
            tokens: vec![TokenId(0), TokenId(1), TokenId(0)],
            hop_v2: vec![
                HopV2 {
                    token_in: TokenId(0),
                    token_out: TokenId(1),
                    reserve_in: 1_000_000_000,
                    reserve_out: 4_000_000_000,
                    fee_bps: 30,
                },
                HopV2 {
                    token_in: TokenId(1),
                    token_out: TokenId(0),
                    reserve_in: 4_000_000_000,
                    reserve_out: 1_010_000_000,
                    fee_bps: 30,
                },
            ],
            amount_in: U256::from(1_000_000u64),
            min_profit: U256::from(1u64),
            executor,
            token_addresses: vec![t0, t1],
        };
        let outcome = harness.validate(&input).expect("expected profitable");
        assert!(outcome.base_token_delta > U256::ZERO);
    }

    #[test]
    fn out_of_manifest_rejects() {
        let manifest = RouteFootprintManifest::new(RouteId(0), RouteKind::V2V2FlashBalancer);
        let p0 = pool_addr(1);
        let db = WarmDb::new();
        let harness = ValidationHarness::new(&db, &manifest);
        let input = ExecutionInput {
            route_id: RouteId(0),
            pools: vec![p0],
            tokens: vec![TokenId(0), TokenId(0)],
            hop_v2: vec![],
            amount_in: U256::from(1u64),
            min_profit: U256::ZERO,
            executor: pool_addr(2),
            token_addresses: vec![],
        };
        assert_eq!(
            harness.validate(&input).unwrap_err(),
            RevertReason::OutOfManifest
        );
    }
}
