//! Route table + CSR pool→route indexes.
//!
//! After startup, the hot path performs a single CSR read to enumerate every hot route
//! touching a dirty pool: `pool_to_hot_route_ids[offsets[p]..offsets[p+1]]`. There are no
//! `HashMap` lookups.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod amount_grid;
pub mod calldata;
pub mod canonical;
pub mod combined;
pub mod compiled;
pub mod csr;
pub mod enumerator;
pub mod fast_path;
pub mod self_impact;
pub mod sizing;
pub mod table;

pub use amount_grid::{AmountGrid, AmountGridLibrary};
pub use calldata::RouteKind;
pub use canonical::{canonicalise_route, RouteCanonKey};
pub use combined::{build_combined, CombineError, CombinedRoute};
pub use csr::{Csr, CsrBuilder};
pub use enumerator::{
    enumerate_k_hop_cycles, enumerate_k_hop_cycles_gated, enumerate_triangles,
    enumerate_triangles_gated, enumerate_two_hop_routes, enumerate_two_hop_routes_gated,
};
pub use fast_path::{RouteDispatch, RouteFastPath};
pub use self_impact::{SelfImpactBuilder, SelfImpactIndex};
pub use sizing::{optimal_size, optimal_size_seeded, SizingResult};
pub use table::{Hop, Route, RouteTable, StrategyKind, MAX_HOPS};
