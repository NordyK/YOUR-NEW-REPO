//! Feed replay/ordering authority — the campaign's §3.1 `StatePoint` contract
//! (codex/CODEX_FIX_PORT_PLAN_2026-07-17.md §3.1; WI-06 plan codex/WI06_PLAN_2026-07-17.md T1).
//!
//! WI-03 landed the two pieces the feed path needed first: the canonical per-leaf feed cursor
//! `(sequence, inner_ordinal)` and the TYPED replay cutoff with explicit `Before`/`Through`
//! boundary semantics (FS-06). WI-06 lands the full vocabulary on top, EXTENDING this module
//! (never a parallel cursor struct in a consumer):
//!
//! - `FeedCursor` (embedding the landed [`FeedReplayCursor`]), `FeedObservation`,
//!   `FeedPointId`, `CanonicalCursor`, `StateOrigin`, `StatePoint`, `StatePointId`;
//! - the partial order [`state_order`] with typed [`StateOrderRelation`] outcomes — ordering
//!   is a PARTIAL order, not "largest block wins": conflict and incomparability are first-class
//!   fail-closed outcomes, and only `Newer` / byte-identical duplicate are admissible;
//! - domain-tagged ID functions (off the hot path — the engine parser copies precomputed IDs,
//!   it never hashes on RX→detect);
//! - the PUBLIC ID-keyed [`ReplayCutoff`] (D4) with resolve-ONCE semantics rejecting
//!   `Conflict`/`Incomparable`/`Stale`, resolving into the private-to-the-drain
//!   [`ResolvedFeedCutoff`] (the landed cursor-keyed enum, de-exported from candidate-facing
//!   use; its FS-06 tests are kept below);
//! - the boot-local NON-AUTHORITATIVE [`TransitionalFeedPoint`] (D3) whose one promotion
//!   surface refuses unconditionally until WI-08 threads the complete precomputed feed
//!   identity.

use core::cmp::Ordering;
use core::num::NonZeroU64;

/// Canonical position of one inner L2 message leaf in the sequencer feed: the feed message
/// number plus the depth-first, left-to-right leaf index the canonical normalizer assigned.
/// Total order: `(sequence, inner_ordinal)` lexicographic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeedReplayCursor {
    /// Sequencer feed message number (`sequenceNumber`).
    pub sequence: u64,
    /// Canonical depth-first leaf index within that message.
    pub inner_ordinal: u32,
}

impl FeedReplayCursor {
    /// Construct a cursor.
    pub const fn new(sequence: u64, inner_ordinal: u32) -> Self {
        Self {
            sequence,
            inner_ordinal,
        }
    }
}

/// Domain-separated identity of the raw feed envelope bytes (computed by the relay/sidecar,
/// never on engine hot cores). Excluded from cross-leg payload equality (`payload_hash`
/// already excludes framing) but part of the immutable `FeedCursor` identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnvelopeIdentity(pub [u8; 32]);

/// Identifies the RPC provider that served an exact-RPC state fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderId(pub u16);

/// §3.1 immutable feed-origin identity: WHICH leaf of WHICH redundancy group, with the
/// canonical decompressed-payload hash that makes independent relay legs comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeedCursor {
    /// Stable identity of the chain/feed redundancy group (one per chain/feed, NOT per leg).
    pub redundancy_group_id: [u8; 16],
    /// The landed WI-03 canonical replay cursor `(sequence, inner_ordinal)`.
    pub replay: FeedReplayCursor,
    /// `keccak256("mantis.feed-payload.v1" || chain_id || redundancy_group_id || sequence ||
    /// canonical_decompressed_l2_message_bytes)` — excludes JSON spelling, WS framing, leg,
    /// and session, so independent legs compare byte-for-byte.
    pub payload_hash: [u8; 32],
    /// Transaction hash of the leaf, when the leaf is a transaction.
    pub tx_hash: Option<[u8; 32]>,
    /// Envelope identity (sidecar-computed).
    pub envelope: EnvelopeIdentity,
}

/// Transport evidence for one observation of a feed leaf by one relay leg. Deliberately
/// EXCLUDED from canonical `StatePoint` identity — used only for per-leg gap/reconnect
/// tracking (§3.1: cross-leg equality is `(redundancy_group_id, sequence, inner_ordinal)` +
/// `payload_hash`; per-leg sessions never enter the ID).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedObservation {
    /// Which relay leg observed the leaf.
    pub leg_id: [u8; 16],
    /// The leg's session epoch (reconnects bump it).
    pub leg_session_epoch: u64,
    /// Local monotonic receive time.
    pub received_mono_ns: u64,
}

/// Domain-tagged identity of a feed point: `keccak256("mantis.feed-point.v1" || chain_id ||
/// immutable FeedCursor)`. The sidecar owns and emits it; the engine parser copies and
/// structurally validates fixed IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeedPointId(pub [u8; 32]);

/// §3.1 canonical-chain position after hash/root verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalCursor {
    /// Canonical L2 block number.
    pub block_number: u64,
    /// Canonical block hash.
    pub block_hash: [u8; 32],
    /// Post-state root of the block.
    pub state_root: [u8; 32],
    /// Transaction index within the block (`None` = whole-block/header point).
    pub tx_index: Option<u32>,
    /// Log index within the transaction (`None` = whole-tx point).
    pub log_index: Option<u32>,
}

/// §3.1 origin of a state mutation — WHO produced the bytes and against WHAT position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateOrigin {
    /// Sequencer-feed leaf (prediction path).
    Feed(FeedCursor),
    /// Canonical header/log watcher after hash/root verification.
    Canonical(CanonicalCursor),
    /// Exact-RPC state fetch: admissible ONLY for the requested block hash and request
    /// generation — a response cannot retag itself.
    ExactRpc {
        /// The cursor the REQUEST named before dispatch.
        requested: CanonicalCursor,
        /// Request generation stamped before dispatch.
        request_generation: u64,
        /// Which provider served it.
        provider_id: ProviderId,
    },
    /// Shadow execution: inherits the job's origin point and adds its execution ordinal.
    Shadow {
        /// The origin point of the shadow job's parent.
        parent: StatePointId,
        /// Execution ordinal within the parent job.
        execution_ordinal: u32,
    },
}

/// §3.1 complete provenance record for one state mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatePoint {
    /// Vocabulary schema version.
    pub schema_version: u16,
    /// Chain ID the point belongs to.
    pub chain_id: u64,
    /// Origin (feed / canonical / exact-RPC / shadow).
    pub origin: StateOrigin,
    /// The L2 block the source REPORTED (unverified; never used for ordering).
    pub reported_l2_block: Option<(u64, [u8; 32])>,
    /// Transport evidence — excluded from identity (per-leg gap tracking only).
    pub observation: Option<FeedObservation>,
    /// Canonical anchor bound by reconciliation (mutable — excluded from identity; bound
    /// separately by `authority_bundle_generation`).
    pub reconciliation_anchor: Option<CanonicalCursor>,
    /// Reconciliation generation (mutable — excluded from identity).
    pub reconciliation_generation: NonZeroU64,
}

/// Domain-tagged identity of a state point. NEVER hashes the full mutable `StatePoint`: it
/// binds the domain tag, `chain_id`, and the IMMUTABLE origin fields only — `FeedObservation`,
/// `reported_l2_block`, `reconciliation_anchor`, and `reconciliation_generation` are excluded,
/// so the same feed leaf keeps the same ID before and after canonical reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatePointId(pub [u8; 32]);

/// §3.1 partial-order relation of a CANDIDATE point relative to a CURRENT point.
/// Only `Newer` and `DuplicateIdentical` are admissible; `Conflict`/`Incomparable` poison the
/// affected authority; `Stale` is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateOrderRelation {
    /// Candidate strictly succeeds current in a comparable lineage.
    Newer,
    /// Candidate is the byte-identical SAME point (idempotent re-delivery).
    DuplicateIdentical,
    /// Candidate strictly precedes current.
    Stale,
    /// Same position, different identity (payload/hash/root) — reorg or split feed.
    Conflict,
    /// No ordering relation exists (cross-origin without a reconciliation binding,
    /// cross-group, cross-parent shadow, ...).
    Incomparable,
}

impl StateOrderRelation {
    /// §3.1: only `Newer` and the byte-identical duplicate are admissible.
    #[inline]
    pub fn admissible(self) -> bool {
        matches!(self, Self::Newer | Self::DuplicateIdentical)
    }
}

/// Explicit reconciliation record binding one feed point to its canonical transaction
/// position. Without one, feed and canonical points are INCOMPARABLE (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconciliationBinding {
    /// The feed point the binding covers (identity-keyed, not bare cursor).
    pub feed: FeedPointId,
    /// The canonical position the feed transaction landed at.
    pub canonical: CanonicalCursor,
    /// Reconciliation generation that produced the binding.
    pub generation: NonZeroU64,
}

// ---------------------------------------------------------------------------------------------
// Domain-tagged ID computation (OFF the hot path: sidecar / canonical watcher / publisher).
// ---------------------------------------------------------------------------------------------

const FEED_POINT_DOMAIN: &[u8] = b"mantis.feed-point.v1";
const STATE_POINT_FEED_DOMAIN: &[u8] = b"mantis.state-point.feed.v1";
const STATE_POINT_CANONICAL_DOMAIN: &[u8] = b"mantis.state-point.canonical.v1";
const STATE_POINT_EXACT_RPC_DOMAIN: &[u8] = b"mantis.state-point.exact-rpc.v1";
const STATE_POINT_SHADOW_DOMAIN: &[u8] = b"mantis.state-point.shadow.v1";

fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        buf.extend_from_slice(p);
    }
    alloy_primitives::keccak256(&buf).0
}

fn encode_opt_hash(dst: &mut Vec<u8>, v: &Option<[u8; 32]>) {
    match v {
        Some(h) => {
            dst.push(1);
            dst.extend_from_slice(h);
        }
        None => dst.push(0),
    }
}

fn encode_opt_u32(dst: &mut Vec<u8>, v: &Option<u32>) {
    match v {
        Some(x) => {
            dst.push(1);
            dst.extend_from_slice(&x.to_be_bytes());
        }
        None => dst.push(0),
    }
}

fn encode_canonical(dst: &mut Vec<u8>, c: &CanonicalCursor) {
    dst.extend_from_slice(&c.block_number.to_be_bytes());
    dst.extend_from_slice(&c.block_hash);
    dst.extend_from_slice(&c.state_root);
    encode_opt_u32(dst, &c.tx_index);
    encode_opt_u32(dst, &c.log_index);
}

/// `FeedPointId = keccak256("mantis.feed-point.v1" || chain_id || immutable FeedCursor)`.
/// Owned/emitted by the sidecar; the engine copies it.
pub fn feed_point_id(chain_id: u64, cursor: &FeedCursor) -> FeedPointId {
    let mut buf = Vec::with_capacity(160);
    buf.extend_from_slice(FEED_POINT_DOMAIN);
    buf.extend_from_slice(&chain_id.to_be_bytes());
    buf.extend_from_slice(&cursor.redundancy_group_id);
    buf.extend_from_slice(&cursor.replay.sequence.to_be_bytes());
    buf.extend_from_slice(&cursor.replay.inner_ordinal.to_be_bytes());
    buf.extend_from_slice(&cursor.payload_hash);
    encode_opt_hash(&mut buf, &cursor.tx_hash);
    buf.extend_from_slice(&cursor.envelope.0);
    FeedPointId(alloy_primitives::keccak256(&buf).0)
}

/// Domain-tagged `StatePointId` for a complete point. Binds the domain tag, schema version,
/// `chain_id`, and the IMMUTABLE origin fields ONLY (§3.1): `FeedObservation`,
/// `reported_l2_block`, `reconciliation_anchor`, and `reconciliation_generation` are
/// deliberately excluded — they are bound separately by `authority_bundle_generation`, so the
/// same feed leaf keeps the same ID before and after canonical reconciliation.
pub fn state_point_id(point: &StatePoint) -> StatePointId {
    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(&point.schema_version.to_be_bytes());
    buf.extend_from_slice(&point.chain_id.to_be_bytes());
    match &point.origin {
        StateOrigin::Feed(cursor) => {
            buf.extend_from_slice(STATE_POINT_FEED_DOMAIN);
            buf.extend_from_slice(&feed_point_id(point.chain_id, cursor).0);
        }
        StateOrigin::Canonical(c) => {
            buf.extend_from_slice(STATE_POINT_CANONICAL_DOMAIN);
            encode_canonical(&mut buf, c);
        }
        StateOrigin::ExactRpc {
            requested,
            request_generation,
            provider_id,
        } => {
            buf.extend_from_slice(STATE_POINT_EXACT_RPC_DOMAIN);
            encode_canonical(&mut buf, requested);
            buf.extend_from_slice(&request_generation.to_be_bytes());
            buf.extend_from_slice(&provider_id.0.to_be_bytes());
        }
        StateOrigin::Shadow {
            parent,
            execution_ordinal,
        } => {
            buf.extend_from_slice(STATE_POINT_SHADOW_DOMAIN);
            buf.extend_from_slice(&parent.0);
            buf.extend_from_slice(&execution_ordinal.to_be_bytes());
        }
    }
    // The mutable tail (`reported_l2_block`, `observation`, `reconciliation_anchor`,
    // `reconciliation_generation`) is NEVER hashed here.
    StatePointId(hash_parts(&[&buf]))
}

/// §3.1: an exact-RPC result is admissible only for the requested block hash and request
/// generation — a response cannot retag itself, and no other origin can claim exact-RPC
/// admissibility.
pub fn exact_rpc_admissible(
    origin: &StateOrigin,
    expected_block_hash: &[u8; 32],
    expected_request_generation: u64,
) -> bool {
    match origin {
        StateOrigin::ExactRpc {
            requested,
            request_generation,
            ..
        } => {
            requested.block_hash == *expected_block_hash
                && *request_generation == expected_request_generation
        }
        StateOrigin::Feed(_) | StateOrigin::Canonical(_) | StateOrigin::Shadow { .. } => false,
    }
}

// ---------------------------------------------------------------------------------------------
// The §3.1 partial order.
// ---------------------------------------------------------------------------------------------

/// Compare two canonical cursors along the (assumed shared) canonical lineage.
/// Same height with a different block hash — or the same block hash with a different state
/// root — is a reorg/corruption `Conflict`, never Newer/Stale. Within one block, positions
/// order by `(tx_index, log_index)` with the whole-block/whole-tx point (`None`) preceding
/// any indexed point.
fn canonical_order(current: &CanonicalCursor, candidate: &CanonicalCursor) -> StateOrderRelation {
    match candidate.block_number.cmp(&current.block_number) {
        Ordering::Greater => StateOrderRelation::Newer,
        Ordering::Less => StateOrderRelation::Stale,
        Ordering::Equal => {
            if candidate.block_hash != current.block_hash {
                return StateOrderRelation::Conflict; // reorg: same height, different hash
            }
            if candidate.state_root != current.state_root {
                return StateOrderRelation::Conflict; // same hash, different root: corruption
            }
            let pos = |c: &CanonicalCursor| {
                (
                    c.tx_index.map_or(0u64, |t| u64::from(t) + 1),
                    c.log_index.map_or(0u64, |l| u64::from(l) + 1),
                )
            };
            match pos(candidate).cmp(&pos(current)) {
                Ordering::Greater => StateOrderRelation::Newer,
                Ordering::Less => StateOrderRelation::Stale,
                Ordering::Equal => StateOrderRelation::DuplicateIdentical,
            }
        }
    }
}

/// The canonical cursor an origin is comparable AT, if any: canonical points at their own
/// cursor; exact-RPC points at the cursor they REQUESTED (a response cannot retag itself).
fn canonical_comparable_cursor(origin: &StateOrigin) -> Option<&CanonicalCursor> {
    match origin {
        StateOrigin::Canonical(c) => Some(c),
        StateOrigin::ExactRpc { requested, .. } => Some(requested),
        StateOrigin::Feed(_) | StateOrigin::Shadow { .. } => None,
    }
}

/// Relation of `candidate` RELATIVE TO `current` under the §3.1 partial order, optionally
/// through an explicit reconciliation binding (the ONLY thing that can make feed and
/// canonical points comparable). Ordering is PARTIAL:
///
/// - feed points compare within the same redundancy group by `(sequence, inner_ordinal)`;
///   the same cursor with a different payload/tx/envelope identity is `Conflict`, and only
///   the byte-identical duplicate is `DuplicateIdentical`;
/// - canonical (and exact-RPC-at-requested-cursor) points compare by [`canonical_order`];
/// - feed and canonical points are `Incomparable` unless `rec` binds EXACTLY the feed point
///   in the pair to a canonical position (then the pair compares through that position);
/// - shadow points compare only against the SAME parent, by execution ordinal;
/// - everything else is `Incomparable` — sequence arithmetic never invents a block number.
pub fn state_order(
    current: &StatePoint,
    candidate: &StatePoint,
    rec: Option<&ReconciliationBinding>,
) -> StateOrderRelation {
    if current.chain_id != candidate.chain_id {
        return StateOrderRelation::Incomparable;
    }
    match (&current.origin, &candidate.origin) {
        (StateOrigin::Feed(a), StateOrigin::Feed(b)) => {
            if a.redundancy_group_id != b.redundancy_group_id {
                return StateOrderRelation::Incomparable;
            }
            match b.replay.cmp(&a.replay) {
                Ordering::Greater => StateOrderRelation::Newer,
                Ordering::Less => StateOrderRelation::Stale,
                Ordering::Equal => {
                    // §3.1: byte-identical is the ONLY admissible duplicate; same cursor
                    // with ANY differing identity component is a split-feed Conflict.
                    if a.payload_hash == b.payload_hash
                        && a.tx_hash == b.tx_hash
                        && a.envelope == b.envelope
                    {
                        StateOrderRelation::DuplicateIdentical
                    } else {
                        StateOrderRelation::Conflict
                    }
                }
            }
        }
        (StateOrigin::Shadow { parent: pa, execution_ordinal: oa },
         StateOrigin::Shadow { parent: pb, execution_ordinal: ob }) => {
            if pa != pb {
                return StateOrderRelation::Incomparable;
            }
            match ob.cmp(oa) {
                Ordering::Greater => StateOrderRelation::Newer,
                Ordering::Less => StateOrderRelation::Stale,
                Ordering::Equal => StateOrderRelation::DuplicateIdentical,
            }
        }
        (a_origin, b_origin) => {
            match (
                canonical_comparable_cursor(a_origin),
                canonical_comparable_cursor(b_origin),
            ) {
                (Some(a), Some(b)) => canonical_order(a, b),
                // Feed vs canonical-comparable: comparable ONLY through an explicit
                // reconciliation binding of EXACTLY the feed point in this pair.
                (None, Some(b)) => {
                    if let (StateOrigin::Feed(fc), Some(binding)) = (a_origin, rec) {
                        if binding.feed == feed_point_id(current.chain_id, fc) {
                            return canonical_order(&binding.canonical, b);
                        }
                    }
                    StateOrderRelation::Incomparable
                }
                (Some(a), None) => {
                    if let (StateOrigin::Feed(fc), Some(binding)) = (b_origin, rec) {
                        if binding.feed == feed_point_id(candidate.chain_id, fc) {
                            return canonical_order(a, &binding.canonical);
                        }
                    }
                    StateOrderRelation::Incomparable
                }
                (None, None) => StateOrderRelation::Incomparable,
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// D3 — boot-local transitional point (NON-AUTHORITATIVE).
// ---------------------------------------------------------------------------------------------

/// Boot-local, NON-AUTHORITATIVE feed trigger record carried by WI-06 publications until
/// WI-08 threads the complete precomputed feed identity (redundancy group, payload hash,
/// envelope identity) from the sidecar. It deliberately has NO conversion into `StatePoint` /
/// `FeedCursor` / `StatePointId`; [`TransitionalFeedPoint::promote_to_state_point`] is the one
/// conversion surface and it must refuse unconditionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionalFeedPoint {
    /// Process boot nonce — a transitional point is meaningless across boots (cross-boot
    /// journal lookups decline).
    pub boot_id: u64,
    /// The landed WI-03 replay cursor.
    pub replay: FeedReplayCursor,
    /// Trigger transaction hash ([0;32] when the trigger has no single tx).
    pub tx_hash: [u8; 32],
}

/// Why a transitional point cannot become an authoritative `StatePoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionalPromotionError {
    /// The complete feed identity (redundancy group / payload hash / envelope) is not
    /// available in-engine; WI-08 is the sole owner of identity threading.
    IdentityUnavailable,
}

impl TransitionalFeedPoint {
    /// The ONE promotion surface — and it fail-closes UNCONDITIONALLY (D3): a transitional
    /// point is boot-local evidence, never authority. The complete feed identity
    /// (redundancy group, payload hash, envelope identity) does not exist in-engine until
    /// WI-08 threads it from the sidecar, and fabricating zeroed identity fields here would
    /// launder a non-authoritative trigger into the canonical vocabulary.
    pub fn promote_to_state_point(
        &self,
        chain_id: u64,
    ) -> Result<StatePoint, TransitionalPromotionError> {
        let _ = chain_id;
        Err(TransitionalPromotionError::IdentityUnavailable)
    }
}

// ---------------------------------------------------------------------------------------------
// D4 — public ID-keyed cutoff with resolve-once rejection; private resolved cursor form.
// ---------------------------------------------------------------------------------------------

/// How a `StatePointId` resolves against the generational point journal at the moment a
/// replay consumer asks (resolve-ONCE: the outcome is used for exactly one drain pass and
/// never cached across authority changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointResolution {
    /// The point is a live feed point at this replay cursor.
    Feed(FeedReplayCursor),
    /// The point is latched in conflict (same cursor, different identity / reorg).
    Conflict,
    /// The point has no ordering relation against the current authority lineage.
    Incomparable,
    /// The point's generation was superseded (stale journal slot).
    Stale,
    /// Only transitional (boot-local, non-authoritative) evidence exists for this ID.
    IdentityUnavailable,
}

/// Why an ID-keyed cutoff refuses to resolve (fail-closed: an unresolvable cutoff
/// authorizes NOTHING).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoffResolveError {
    /// The named point is latched in conflict.
    Conflict,
    /// The named point is incomparable with the current authority lineage.
    Incomparable,
    /// The named point's journal slot was superseded.
    Stale,
    /// Only non-authoritative transitional evidence exists.
    IdentityUnavailable,
}

/// §3.1 PUBLIC typed replay cutoff (D4): ID-keyed — a replay consumer names the state POINT
/// (identity), not a bare cursor, and must state whether the named leaf itself is excluded
/// (`Before`) or included (`Through`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayCutoff {
    /// Replay strictly BEFORE the named point: the point's leaf itself is excluded.
    Before(StatePointId),
    /// Replay up to AND INCLUDING the named point's leaf.
    Through(StatePointId),
}

impl ReplayCutoff {
    /// The named point of this cutoff.
    pub fn point(&self) -> StatePointId {
        match self {
            Self::Before(id) | Self::Through(id) => *id,
        }
    }

    /// Resolve ONCE into the drain's private cursor form. Rejects `Conflict`,
    /// `Incomparable`, `Stale`, and `IdentityUnavailable` — an unresolvable cutoff must
    /// authorize nothing (fail-closed), never "resolve anyway": a fabricated cursor for a
    /// conflicted/stale point would silently re-open exactly the replay window the typed
    /// cutoff exists to close.
    pub fn resolve_with(
        &self,
        resolve: impl FnOnce(&StatePointId) -> PointResolution,
    ) -> Result<ResolvedFeedCutoff, CutoffResolveError> {
        let id = self.point();
        let cursor = match resolve(&id) {
            PointResolution::Feed(cursor) => cursor,
            PointResolution::Conflict => return Err(CutoffResolveError::Conflict),
            PointResolution::Incomparable => return Err(CutoffResolveError::Incomparable),
            PointResolution::Stale => return Err(CutoffResolveError::Stale),
            PointResolution::IdentityUnavailable => {
                return Err(CutoffResolveError::IdentityUnavailable)
            }
        };
        Ok(match self {
            Self::Before(_) => ResolvedFeedCutoff::Before(cursor),
            Self::Through(_) => ResolvedFeedCutoff::Through(cursor),
        })
    }
}

/// RESOLVED cursor-keyed cutoff — the drain-internal form (WI-03's landed enum, renamed per
/// D4). Candidate-facing consumers hold the ID-keyed [`ReplayCutoff`]; only a successful
/// [`ReplayCutoff::resolve_with`] mints this, so a conflicted/stale/incomparable point can
/// never reach `admits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedFeedCutoff {
    /// Replay strictly BEFORE the named leaf: the leaf itself is excluded.
    Before(FeedReplayCursor),
    /// Replay up to AND INCLUDING the named leaf (a candidate's own trigger is the last
    /// eligible event).
    Through(FeedReplayCursor),
}

impl ResolvedFeedCutoff {
    /// Whether the leaf at `(sequence, inner_ordinal)` is eligible under this cutoff:
    /// lexicographic comparison against the named cursor, with `Before` excluding and
    /// `Through` including the boundary leaf itself.
    pub fn admits(&self, sequence: u64, inner_ordinal: u32) -> bool {
        let leaf = FeedReplayCursor::new(sequence, inner_ordinal);
        match self {
            Self::Before(cursor) => leaf < *cursor,
            Self::Through(cursor) => leaf <= *cursor,
        }
    }

    /// The named cursor (boundary leaf) of this cutoff.
    pub fn cursor(&self) -> FeedReplayCursor {
        match self {
            Self::Before(cursor) | Self::Through(cursor) => *cursor,
        }
    }
}

/// Compare two feed leaves in canonical replay order.
pub fn feed_replay_order(a: FeedReplayCursor, b: FeedReplayCursor) -> Ordering {
    a.cmp(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(tag: u8) -> [u8; 16] {
        [tag; 16]
    }

    fn feed_cursor(tag: u8, seq: u64, ord: u32, payload: u8) -> FeedCursor {
        FeedCursor {
            redundancy_group_id: group(tag),
            replay: FeedReplayCursor::new(seq, ord),
            payload_hash: [payload; 32],
            tx_hash: Some([0xAA; 32]),
            envelope: EnvelopeIdentity([0xEE; 32]),
        }
    }

    fn feed_point(tag: u8, seq: u64, ord: u32, payload: u8) -> StatePoint {
        StatePoint {
            schema_version: 1,
            chain_id: 42161,
            origin: StateOrigin::Feed(feed_cursor(tag, seq, ord, payload)),
            reported_l2_block: None,
            observation: None,
            reconciliation_anchor: None,
            reconciliation_generation: NonZeroU64::MIN,
        }
    }

    fn canonical_cursor(block: u64, hash: u8, root: u8) -> CanonicalCursor {
        CanonicalCursor {
            block_number: block,
            block_hash: [hash; 32],
            state_root: [root; 32],
            tx_index: None,
            log_index: None,
        }
    }

    fn canonical_point(block: u64, hash: u8, root: u8) -> StatePoint {
        StatePoint {
            schema_version: 1,
            chain_id: 42161,
            origin: StateOrigin::Canonical(canonical_cursor(block, hash, root)),
            reported_l2_block: None,
            observation: None,
            reconciliation_anchor: None,
            reconciliation_generation: NonZeroU64::MIN,
        }
    }

    // ------------------------------------------------------------------------------------
    // WI-06 T1 stub-then-assert REDs (plan rev 4 T1; semantics from spec §3.1).
    // ------------------------------------------------------------------------------------

    /// §3.1: the same feed cursor with a different payload hash is `Conflict`, NOT a
    /// duplicate — two legs disagreeing about one leaf's bytes is a split feed, and treating
    /// it as an idempotent duplicate silently trusts whichever leg arrived first.
    #[test]
    fn feed_points_conflict_on_same_cursor_different_payload_hash() {
        let a = feed_point(1, 700, 2, 0x11);
        let b = feed_point(1, 700, 2, 0x22);
        assert_eq!(
            state_order(&a, &b, None),
            StateOrderRelation::Conflict,
            "same (group, sequence, inner_ordinal), different payload_hash MUST be Conflict"
        );
        assert!(
            !state_order(&a, &b, None).admissible(),
            "a conflicted point must never be admissible"
        );
    }

    /// §3.1: feed and canonical points are INCOMPARABLE until an explicit reconciliation
    /// record binds the feed cursor to a canonical transaction position. With the binding,
    /// they become comparable through it.
    #[test]
    fn feed_and_canonical_points_incomparable_without_reconciliation_binding() {
        let feed = feed_point(1, 700, 2, 0x11);
        let canon = canonical_point(1_000_000, 0x33, 0x44);
        assert_eq!(
            state_order(&feed, &canon, None),
            StateOrderRelation::Incomparable,
            "feed vs canonical WITHOUT a reconciliation binding must be Incomparable — \
             sequence arithmetic never invents a block number"
        );
        // With an explicit binding of THIS feed point at an EARLIER canonical position, the
        // later canonical point is Newer.
        let binding = ReconciliationBinding {
            feed: match &feed.origin {
                StateOrigin::Feed(c) => feed_point_id(feed.chain_id, c),
                _ => unreachable!(),
            },
            canonical: canonical_cursor(999_999, 0x55, 0x66),
            generation: NonZeroU64::MIN,
        };
        assert_eq!(
            state_order(&feed, &canon, Some(&binding)),
            StateOrderRelation::Newer,
            "with a binding at block 999_999, the canonical point at 1_000_000 is Newer"
        );
        // A binding for a DIFFERENT feed point must not make this pair comparable.
        let other = feed_point(1, 700, 3, 0x11);
        let unrelated = ReconciliationBinding {
            feed: match &other.origin {
                StateOrigin::Feed(c) => feed_point_id(other.chain_id, c),
                _ => unreachable!(),
            },
            canonical: canonical_cursor(999_999, 0x55, 0x66),
            generation: NonZeroU64::MIN,
        };
        assert_eq!(
            state_order(&feed, &canon, Some(&unrelated)),
            StateOrderRelation::Incomparable,
            "a binding for a DIFFERENT feed point must not comparabilize this pair"
        );
    }

    /// §3.1: canonical points at the same height with different hashes are a REORG
    /// conflict, never Newer/Stale/duplicate.
    #[test]
    fn canonical_same_height_different_hash_is_reorg_conflict() {
        let a = canonical_point(1_000_000, 0x11, 0x22);
        let b = canonical_point(1_000_000, 0x33, 0x22);
        assert_eq!(
            state_order(&a, &b, None),
            StateOrderRelation::Conflict,
            "same height, different block hash MUST be a reorg Conflict"
        );
        // Same hash but different state root is equally a conflict (corrupted source).
        let c = canonical_point(1_000_000, 0x11, 0x99);
        assert_eq!(
            state_order(&a, &c, None),
            StateOrderRelation::Conflict,
            "same block hash with a different state root MUST be Conflict"
        );
    }

    /// §3.1: exact-RPC results are admissible only for the requested block hash and request
    /// generation — a response cannot retag itself.
    #[test]
    fn exact_rpc_admissible_only_for_requested_hash_and_generation() {
        let origin = StateOrigin::ExactRpc {
            requested: canonical_cursor(1_000_000, 0x11, 0x22),
            request_generation: 7,
            provider_id: ProviderId(2),
        };
        assert!(
            exact_rpc_admissible(&origin, &[0x11; 32], 7),
            "matching hash + generation is admissible"
        );
        assert!(
            !exact_rpc_admissible(&origin, &[0x12; 32], 7),
            "a response for a DIFFERENT block hash must be refused"
        );
        assert!(
            !exact_rpc_admissible(&origin, &[0x11; 32], 8),
            "a response from a SUPERSEDED request generation must be refused"
        );
        assert!(
            !exact_rpc_admissible(
                &StateOrigin::Canonical(canonical_cursor(1_000_000, 0x11, 0x22)),
                &[0x11; 32],
                7
            ),
            "a non-exact-RPC origin can never claim exact-RPC admissibility"
        );
    }

    /// §3.1: `StatePointId` binds the domain tag + chain + IMMUTABLE origin only. The same
    /// feed leaf keeps the same ID before and after reconciliation; transport observation and
    /// the reported (unverified) L2 block never enter the ID.
    #[test]
    fn state_point_id_excludes_observation_and_reconciliation_mutables() {
        let bare = feed_point(1, 700, 2, 0x11);
        let mut reconciled = bare;
        reconciled.observation = Some(FeedObservation {
            leg_id: [7; 16],
            leg_session_epoch: 3,
            received_mono_ns: 123_456,
        });
        reconciled.reported_l2_block = Some((23_000_700, [0xBB; 32]));
        reconciled.reconciliation_anchor = Some(canonical_cursor(23_000_700, 0xBB, 0xCC));
        reconciled.reconciliation_generation = NonZeroU64::new(9).expect("nonzero");
        assert_eq!(
            state_point_id(&bare),
            state_point_id(&reconciled),
            "the same feed leaf MUST keep the same StatePointId across reconciliation — \
             observation/reported-block/reconciliation mutables are bound by \
             authority_bundle_generation, never by the ID"
        );
        // Different immutable identity ⇒ different ID.
        let other_leaf = feed_point(1, 700, 3, 0x11);
        assert_ne!(state_point_id(&bare), state_point_id(&other_leaf));
        let other_payload = feed_point(1, 700, 2, 0x22);
        assert_ne!(state_point_id(&bare), state_point_id(&other_payload));
    }

    /// §3.1: only a BYTE-IDENTICAL duplicate is admissible as a duplicate. Same cursor +
    /// same payload but a different envelope identity (or tx hash) is NOT "the same point
    /// again" — it is a Conflict.
    #[test]
    fn byte_identical_duplicate_is_the_only_admissible_duplicate() {
        let a = feed_point(1, 700, 2, 0x11);
        let b = feed_point(1, 700, 2, 0x11);
        assert_eq!(
            state_order(&a, &b, None),
            StateOrderRelation::DuplicateIdentical,
            "byte-identical re-delivery is the idempotent duplicate"
        );
        assert!(state_order(&a, &b, None).admissible());

        let mut different_envelope = b;
        if let StateOrigin::Feed(c) = &mut different_envelope.origin {
            c.envelope = EnvelopeIdentity([0xDD; 32]);
        }
        assert_eq!(
            state_order(&a, &different_envelope, None),
            StateOrderRelation::Conflict,
            "same cursor+payload with a DIFFERENT envelope identity must be Conflict, \
             not an admissible duplicate"
        );

        let mut different_tx = b;
        if let StateOrigin::Feed(c) = &mut different_tx.origin {
            c.tx_hash = Some([0xAB; 32]);
        }
        assert_eq!(
            state_order(&a, &different_tx, None),
            StateOrderRelation::Conflict,
            "same cursor+payload with a DIFFERENT tx hash must be Conflict"
        );
    }

    /// D4: the ID-keyed cutoff resolves ONCE and REJECTS `Conflict`/`Incomparable`/`Stale`
    /// (and transitional identity-unavailable) — a cutoff whose named point is not live feed
    /// authority must authorize NOTHING, never resolve to a fabricated cursor.
    #[test]
    fn id_keyed_cutoff_resolution_rejects_conflict_incomparable_and_stale() {
        let id = StatePointId([0x77; 32]);
        let cutoff = ReplayCutoff::Through(id);

        let ok = cutoff.resolve_with(|_| PointResolution::Feed(FeedReplayCursor::new(700, 2)));
        assert_eq!(
            ok,
            Ok(ResolvedFeedCutoff::Through(FeedReplayCursor::new(700, 2))),
            "a live feed point resolves to its cursor with the SAME boundary semantics"
        );

        assert_eq!(
            cutoff.resolve_with(|_| PointResolution::Conflict),
            Err(CutoffResolveError::Conflict),
            "a conflicted point must REFUSE resolution"
        );
        assert_eq!(
            cutoff.resolve_with(|_| PointResolution::Incomparable),
            Err(CutoffResolveError::Incomparable),
            "an incomparable point must REFUSE resolution"
        );
        assert_eq!(
            cutoff.resolve_with(|_| PointResolution::Stale),
            Err(CutoffResolveError::Stale),
            "a stale (superseded-generation) point must REFUSE resolution"
        );
        assert_eq!(
            ReplayCutoff::Before(id).resolve_with(|_| PointResolution::IdentityUnavailable),
            Err(CutoffResolveError::IdentityUnavailable),
            "transitional-only identity must REFUSE resolution (non-authoritative until WI-08)"
        );
    }

    /// D3: the transitional boot-local point is evidence, never authority — its one
    /// promotion surface refuses UNCONDITIONALLY (WI-08 owns identity threading).
    #[test]
    fn transitional_feed_point_cannot_convert_into_canonical_state_point() {
        let transitional = TransitionalFeedPoint {
            boot_id: 0xB007,
            replay: FeedReplayCursor::new(700, 2),
            tx_hash: [0xAA; 32],
        };
        assert_eq!(
            transitional.promote_to_state_point(42161),
            Err(TransitionalPromotionError::IdentityUnavailable),
            "a TransitionalFeedPoint must NEVER promote into a StatePoint — the complete \
             feed identity (redundancy group / payload hash / envelope) does not exist \
             in-engine until WI-08 threads it from the sidecar"
        );
    }

    // ------------------------------------------------------------------------------------
    // Landed WI-03 tests — KEPT (D4 renamed the cursor-keyed enum to ResolvedFeedCutoff).
    // ------------------------------------------------------------------------------------

    /// FS-06: all siblings of one batch share the outer frame's receive timestamp, so the old
    /// `(seq, recv_ns)` cutoff admitted EVERY sibling once it admitted one — including leaves
    /// AFTER the candidate's own trigger. The typed cutoff must cut exactly at the ordinal.
    #[test]
    fn same_sequence_siblings_cutoff_exactly_by_inner_ordinal() {
        let cutoff = ResolvedFeedCutoff::Through(FeedReplayCursor::new(700, 1));
        assert!(cutoff.admits(699, 9), "earlier sequence always eligible");
        assert!(cutoff.admits(700, 0), "earlier sibling eligible");
        assert!(cutoff.admits(700, 1), "the named trigger leaf is included by Through");
        assert!(
            !cutoff.admits(700, 2),
            "a LATER same-sequence sibling must be excluded — the old (seq, recv_ns) cutoff \
             replayed it into the fork before validating the candidate (FS-06)"
        );
        assert!(!cutoff.admits(701, 0), "later sequence never eligible");
    }

    /// §3.1: `Before` and `Through` must have explicitly OPPOSITE semantics at the named leaf.
    #[test]
    fn replay_cutoff_before_and_through_have_explicit_opposite_boundary_semantics() {
        let leaf = FeedReplayCursor::new(41, 3);
        let before = ResolvedFeedCutoff::Before(leaf);
        let through = ResolvedFeedCutoff::Through(leaf);

        assert!(
            !before.admits(41, 3),
            "Before(leaf) must EXCLUDE the named leaf itself"
        );
        assert!(
            through.admits(41, 3),
            "Through(leaf) must INCLUDE the named leaf itself"
        );
        // Away from the boundary the two agree.
        assert!(before.admits(41, 2) && through.admits(41, 2));
        assert!(!before.admits(41, 4) && !through.admits(41, 4));
        assert!(before.admits(40, u32::MAX) && through.admits(40, u32::MAX));
        assert!(!before.admits(42, 0) && !through.admits(42, 0));
    }
}
