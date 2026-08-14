//! Depth and optimality certificates (§10.8 / §10.9).

use alloy_primitives::U256;

use engine_state::{Generation, RouteId};

/// Solver kind recorded in the optimality certificate.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolverKind {
    /// Closed-form analytical solution.
    ClosedForm = 0,
    /// Grid scan only.
    GridOnly = 1,
    /// Bounded golden-section over the grid bracket.
    GoldenSection = 2,
    /// Background replay (never live-submit eligible).
    ReplayBackground = 3,
}

/// `DepthCertificate` (§10.8).
#[derive(Debug, Clone)]
pub struct DepthCertificate {
    /// Route id.
    pub route_id: RouteId,
    /// Selected input.
    pub amount_in: U256,
    /// Expected output after AMM fees and exact rounding.
    pub expected_out: U256,
    /// `min_amount_out` lower bound enforced by the executor.
    pub min_amount_out: U256,
    /// `min_net_profit` enforced by the executor's final balance check.
    pub min_net_profit: U256,
    /// Route-family safe input cap.
    pub max_safe_amount: U256,
    /// Bottleneck route depth.
    pub bottleneck_depth: U256,
    /// Generations seen for every touched pool (parallel slice with route hops).
    pub state_generations: Vec<Generation>,
    /// Warm-range proof bitmask (one bit per hop needing a window).
    pub warm_range_proof: u64,
}

/// `OptimalityCertificate` (§10.9).
#[derive(Debug, Clone, Copy)]
pub struct OptimalityCertificate {
    /// Solver used.
    pub solver_kind: SolverKind,
    /// Search bracket low.
    pub search_low: U256,
    /// Search bracket high.
    pub search_high: U256,
    /// Best input selected.
    pub best_amount: U256,
    /// Net profit at the selected amount.
    pub best_net_profit: U256,
    /// Neighbour-probe profit (left).
    pub left_probe_profit: U256,
    /// Neighbour-probe profit (right).
    pub right_probe_profit: U256,
    /// Iterations consumed.
    pub iterations: u32,
    /// Iteration cap.
    pub iteration_cap: u32,
}

impl OptimalityCertificate {
    /// Construct from a `SizingOutcome`-style result.
    pub fn from_grid_only(bracket: (U256, U256), best_amount: U256, best_net_profit: U256) -> Self {
        Self {
            solver_kind: SolverKind::GridOnly,
            search_low: bracket.0,
            search_high: bracket.1,
            best_amount,
            best_net_profit,
            left_probe_profit: U256::ZERO,
            right_probe_profit: U256::ZERO,
            iterations: 1,
            iteration_cap: 1,
        }
    }
}
