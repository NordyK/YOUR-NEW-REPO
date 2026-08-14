//! Engine state: dense IDs, pool table, AMM state stores, seqlock publication.
//!
//! Every entity that appears on the hot path lives behind a dense numeric ID built at startup
//! (`TokenId`, `PoolId`, `RouteId`, `PairId`, `VenueId`). Hot maps are contiguous `Vec`s indexed
//! by ID; no `HashMap` lookups on the hot path. State publication uses a seqlock pattern so
//! single-writer updates are visible to readers without locks and without torn reads.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// clippy: byte-exact state stores legitimately use many-arg constructors, large per-family state
// enums, and parallel-array indexing; the rest are doc/style nits. Zero-behavior suppression on
// certified hot-path state code.
#![allow(
    clippy::too_many_arguments,
    clippy::large_enum_variant,
    clippy::needless_range_loop,
    clippy::type_complexity,
    clippy::doc_lazy_continuation
)]

pub mod authority;
pub mod balancer_state;
pub mod balancer_weighted_state;
pub mod builder;
pub mod curve_crypto_state;
pub mod curve_state;
pub mod dodo_state;
pub mod fluid_state;
pub mod ids;
pub mod lb_state;
pub mod maverick_state;
pub mod pool;
pub mod publication;
pub mod registry;
pub mod token;
pub mod v2_state;
pub mod v3_state;

pub use balancer_state::{BalancerStableSlot, BalancerStateStore};
pub use balancer_weighted_state::{BalancerWeightedSlot, BalancerWeightedStateStore};
pub use builder::{Universe, UniverseBuilder};
pub use curve_crypto_state::{CryptoCurveSlot, CryptoCurveStateStore};
pub use curve_state::{CurveSlot, CurveStateStore};
pub use dodo_state::DodoStateStore;
pub use fluid_state::{FluidPoolSnapshot, FluidStateStore};
pub use ids::{Generation, PairId, PoolId, RouteId, StateSlot, TemplateId, TokenId, VenueId};
pub use lb_state::{LbSlot, LbStateStore};
pub use maverick_state::MaverickStateStore;
pub use pool::{PoolKind, PoolMeta, PoolTable, PoolTier, PARAM_POISON_HARD, PARAM_POISON_NONE, PARAM_POISON_SOFT};
pub use registry::{AddressRegistry, RegistryBuilder, RegistryError};
pub use token::{Address, RiskClass, TokenMeta, TokenTable};
pub use v2_state::{V2Reserves256, V2State, V2StateStore};
pub use v3_state::{decode_v3_liquidity_event, decode_v4_modify_liquidity, V3State, V3StateStore};
