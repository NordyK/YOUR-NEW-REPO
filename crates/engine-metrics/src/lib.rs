//! Metrics: per-thread cache-padded counters scraped by the background core. Histograms
//! use `hdrhistogram` and live on the background core only. Also exposes the §18.7
//! `CountingAllocator` used to enforce the hot-path zero-allocation gate.

#![deny(missing_docs)]

pub mod alloc_counter;
pub mod breach_monitor;
pub mod counters;
pub mod histograms;
pub mod kill_switch;
pub mod reasons;
pub mod tail_attrib;

pub use alloc_counter::{assert_zero_allocations, CountingAllocator};
pub use breach_monitor::{
    build_webhook_payload, pager_from_env, BreachThresholds, LogPager, MultiPager, Pager,
    RateLimitedPager, TelemetryBreachMonitor, WebhookFormat, WebhookPager,
};
pub use counters::{Counter, CountersArray};
pub use histograms::{
    bind_thread_shard_to_core, current_thread_shard, metrics_enabled, set_metrics_enabled,
    shard_for_core, LatencyHistograms, StageLatency, StageSnapshot,
};
pub use kill_switch::{KillSwitch, KillSwitches};
pub use reasons::{ReasonRing, REJECTION_REASON_COUNT};
