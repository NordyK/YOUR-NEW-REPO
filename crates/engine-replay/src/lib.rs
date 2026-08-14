//! Deterministic replay harness. Reads a captured feed dump (one JSON envelope per line),
//! pushes each frame through the full pipeline, and asserts that:
//! * Every parsed envelope's `sequenceNumber` is monotonically increasing.
//! * Dirty-route detection produces consistent counts across runs.
//! * Profit math is deterministic for identical input states.
//!
//! This crate provides the driver. Higher-level engine integration consumes it via a
//! shared trait surface.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod corpus;
pub mod driver;

pub use corpus::{FeedFrame, FeedFrameSource};
pub use driver::{ReplayDriver, ReplayFrameTiming, ReplayStats};
