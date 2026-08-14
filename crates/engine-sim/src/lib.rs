//! Sizing kernels and §12 profit math.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod certificate;
pub mod profit;
pub mod sizing;

pub use certificate::{DepthCertificate, OptimalityCertificate, SolverKind};
pub use profit::{
    canonical_net_profit, ev_decision, expected_value, EvDecision, EvDecomposition, ProfitTerms,
    SubmissionDecision,
};
pub use sizing::{
    bounded_golden_section_v2v2, closed_form_v2v2, grid_search_v2v2, grid_search_v3v3,
    SizingOutcome,
};
