//! Fluid AMM kernel (folded in from the former `fluid-amm-kernel` crate).
//!
//! A math-only, host-agnostic quoting kernel for Fluid DEX v1/T1 and Fluid DEX v2
//! D3/D4 style range-order pools. Deterministic integer math with EVM-style
//! `U256`/`U512` arithmetic; no RPC, ABI, or discovery — the host owns pool
//! discovery, resolver calls, log processing, calldata, and submission. This
//! module only answers: "given a refreshed Fluid state snapshot, what is the
//! swap math result?"
//!
//! Folded in verbatim from the standalone kernel crate, which did not enforce
//! `missing_docs`; scope the allow here rather than annotate ~180 math items.
#![allow(missing_docs)]

pub mod error;
pub mod fixed;
pub mod fluid_v1;
pub mod fluid_v2;
pub mod kernel;
pub mod types;

pub use alloy_primitives::U256;
pub use error::{AmmError, Result};
pub use fluid_v1::*;
pub use fluid_v2::*;
pub use kernel::*;
pub use types::*;
