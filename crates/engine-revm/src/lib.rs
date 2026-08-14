//! REVM warm-DB wrapper and route-footprint validation.
//!
//! The production engine uses [`revm`](https://github.com/bluealloy/revm) 40.x as the canonical
//! local execution engine. To keep this crate buildable in any environment (the upstream
//! `revm` LLVM toolchain and dependency set is heavy), we ship the warm-DB abstraction and
//! a deterministic *simulation harness* that re-executes the engine's exact AMM kernels with
//! a footprint manifest, ensuring the same fail-closed semantics (§10.10).
//!
//! Two validation paths exist, and it matters which enforces what:
//!
//! * The **REVM `ForkExecutor`** (`fork_exec.rs`) is the AUTHORITATIVE path: it deploys and executes
//!   the real `MantisExecutor` bytecode over a pinned fork, so callback authentication, flash-loan
//!   repayment, and `minNetProfit`/`Loss` reverts are enforced by the CONTRACT itself (not by this
//!   crate). A `Revert(Loss/InsufficientProfit)` / `Halt` → reject; a payee balance-delta ≤ 0 →
//!   reject. This is what gates real capital.
//! * The math-only **`ValidationHarness`** (`harness.rs`) is a FALLBACK/divergence check: it is
//!   account-manifest scoped and gross-oriented (`gross_out ≥ amount_in + min_profit`). It does
//!   NOT itself execute callbacks or assert flash-loan repayment — those properties come from the
//!   ForkExecutor path above. Do not rely on the fallback alone for capital safety.
//!
//! Storage access under the ForkExecutor is restricted to the manifest's allowlist; out-of-manifest
//! access yields `OutOfManifest` and demotes the route (§0.4 rule 17, §10.10).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cl_slots;
pub mod compiled;
pub mod erc20_slots;
pub mod feed_executor;
pub mod fork_db;
pub mod fork_exec;
pub mod harness;
pub mod manifest;
pub mod receipt;
pub mod revm_validator;
/// Persistent revmc AOT artifact cache (Stage 3) — only compiled with `--features revmc`.
#[cfg(feature = "revmc")]
pub mod revmc_artifacts;
pub mod warm_db;

pub use feed_executor::FeedExecutor;
pub use fork_db::{CanonicalFetchError, CanonicalFetchErrorKind, ForkDb, Proven, StateFetcher};
pub use fork_exec::{ChangedSlot, ForkExecutor, ProbeMisses, ReplayOutcome};
pub use harness::{ExecutionInput, ExecutionOutcome, HopV2, RevertReason, ValidationHarness};
pub use manifest::{CallbackAuth, ExternalCall, RouteFootprintManifest};
pub use receipt::{LocalReceipt, LocalReceiptCache};
pub use revm_validator::{RevmCallResult, RevmError, RevmValidator};
pub use warm_db::{TokenBalance, WarmDb};
