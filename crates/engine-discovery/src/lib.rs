//! Plan §5.4 gate 1 (Discovered) — factory event-log discovery of every pool on Arbitrum
//! One for the configured allowlist of factories and tokens.
//!
//! The discovery crate is **background-only**. The hot path never calls into it. Discovery
//! runs at startup (and on a 5–30 min refresh cadence per §6) on a dedicated background
//! thread; it streams `PoolCreated` (V3 family) and `PairCreated` (V2 family) events via
//! chunked `eth_getLogs`, decodes the indexed topics + data field, filters by token
//! allowlist, and emits `DiscoveredPool` records that subsequent gates (§5.4 gates 2–8)
//! pull through validation, snapshot, score, and election.
//!
//! Per the plan, this replaces the static `engine.json` `pools` array: the engine no
//! longer trusts a hardcoded pool list ("No static pool whitelist" — §2116).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// AUD-DISCOVERY-CAP: upper bound on the pool count an enumerating factory (`poolCount()` /
/// `pool_count()`) is allowed to drive an allocation or loop with. Arbitrum's largest real factory
/// has O(10^4) pools; capping at 200k rejects a hostile/buggy RPC that returns an astronomical count
/// (which would otherwise `Vec::with_capacity` into an OOM abort or spin an unbounded `eth_call`
/// loop). Background/cold path only.
pub const MAX_DISCOVERY_POOLS: usize = 200_000;

/// Clamp an RPC-supplied pool count to [`MAX_DISCOVERY_POOLS`]. Pure; unit-tested.
#[inline]
pub fn capped_pool_count(raw: usize) -> usize {
    raw.min(MAX_DISCOVERY_POOLS)
}

#[cfg(test)]
mod discovery_cap_tests {
    #[test]
    fn capped_pool_count_bounds_hostile_values() {
        assert_eq!(super::capped_pool_count(42), 42, "real counts pass through");
        assert_eq!(
            super::capped_pool_count(usize::MAX),
            super::MAX_DISCOVERY_POOLS,
            "astronomical count is clamped, not trusted"
        );
    }
}

pub mod codehash_artifacts;
pub mod codehash_gate;
pub mod factory_algebra;
pub mod factory_balancer;
pub mod factory_curve;
pub mod factory_dodo;
pub mod factory_lb;
pub mod factory_maverick;
pub mod factory_solidly;
pub mod factory_v2;
pub mod factory_v3;
pub mod factory_v4;
pub mod fluid_resolver;
pub mod logs;
pub mod score;
pub mod telemetry;
pub mod types;

pub use codehash_artifacts::{load_allowlist_from_dir, CodehashRecorder, RECORDED_FAMILIES};
pub use codehash_gate::{validate_pool_codehash, CodehashAllowlist};
pub use factory_algebra::{discover_algebra_pools, AlgebraFactoryConfig, ALGEBRA_POOL_TOPIC};
pub use factory_balancer::{discover_balancer_pools, BalancerVaultConfig};
pub use factory_curve::{discover_curve_pools, CurveFactoryConfig};
pub use factory_dodo::{
    discover_dodo_pools, new_dpp_topic, new_dsp_topic, new_dvm_topic, DodoFactoryConfig,
};
pub use factory_lb::{discover_lb_pools, LbFactoryConfig, LB_PAIR_CREATED_TOPIC};
pub use factory_maverick::{discover_maverick_pools, MaverickFactoryConfig};
pub use factory_solidly::{discover_solidly_pools, SolidlyFactoryConfig};
pub use factory_v2::{discover_v2_pools, V2FactoryConfig, V2_PAIR_CREATED_TOPIC};
pub use factory_v3::{discover_v3_pools, V3FactoryConfig, V3_POOL_CREATED_TOPIC};
pub use factory_v4::{
    discover_v4_pools, v4_pool_id_for, V4DiscoveredPool, V4PoolManagerConfig, V4_INITIALIZE_TOPIC,
};
pub use fluid_resolver::{
    discover_fluid_v1_pools, discover_fluid_v2_pools, FluidResolverConfig, FluidVariant,
};
pub use logs::{fetch_logs_chunked, LogEntry};
pub use score::{route_winnability_q32, score_pool, PoolScoreInputs, PoolScoreOutputs, TokenRole};
pub use telemetry::PoolTelemetry;
pub use types::{DiscoveredPool, DiscoveryError};
