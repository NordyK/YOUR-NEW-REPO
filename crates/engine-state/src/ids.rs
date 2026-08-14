//! Dense numeric IDs used on the hot path.

/// Token index in the dense token table.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct TokenId(pub u16);

/// Pair index (unordered pair of tokens).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct PairId(pub u16);

/// Pool index in the dense pool table.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct PoolId(pub u32);

/// Route index in the dense route table.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct RouteId(pub u32);

/// Venue (DEX-family + factory) index.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct VenueId(pub u16);

/// Index into a family-specific state-store dense vector.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct StateSlot(pub u32);

/// Calldata-template index.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct TemplateId(pub u32);

/// Monotonic generation number used as the seqlock counter.
pub type Generation = u64;
