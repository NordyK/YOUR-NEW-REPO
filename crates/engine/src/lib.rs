//! Engine library surface used by integration tests, benches, and operator tooling.
//!
//! The binary entrypoint lives in `main.rs`; this module re-exports the pipeline
//! orchestrator so external code (tests, benches) can drive it without reaching into
//! private types.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// The candidate-record `serde_json::json!{}` literal (pipeline.rs) carries the full standalone-replay
// schema (~40 fields); the macro expands one recursion level per field, exceeding the default 128.
#![recursion_limit = "512"]

pub mod campaign_oracle;
pub mod dashboard;
pub mod diag;
pub mod discovery_cache;
pub mod fastlane_policy;
pub mod fluid_poller;
pub mod fork_warmer;
pub mod freshness_audit;
pub mod gas_refresher;
pub mod latency_budgets;
pub mod latency_probe;
pub mod mantis_submit;
pub mod mirror_journal;
pub mod mirror_journal_persist;
pub mod mpt_verifier;
pub mod paper_mode;
pub mod paper_reconcile_cli;
pub mod pin_drift;
pub mod pipeline;
pub mod receipt_sampler;
pub mod revm_fork;
pub mod revm_replay;
pub mod route_readsets;
pub mod run_manifest;
pub mod runtime;
pub mod runtime_policy;
pub mod scorer;
pub mod seq_latency_monitor;
pub mod shadow_revm;
pub mod shadow_state;
pub mod spot_cache;
pub mod startup;
pub mod state_mirror;
pub mod token_pricing;
pub mod warmup;
