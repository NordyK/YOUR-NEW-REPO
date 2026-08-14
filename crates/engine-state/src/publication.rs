//! §3.3 sole-writer transaction-epoch publication substrate + §3.2 route-authority seqlock
//! cells (OBSERVE-ONLY in WI-06) + the D1/D1b/D2 fail-closed authority primitives.
//! (codex/CODEX_FIX_PORT_PLAN_2026-07-17.md §3.2/§3.3; WI-06 plan codex/WI06_PLAN_2026-07-17.md
//! T2 — this is the spec-named NEW file; every other WI-06 change is fix-in-place.)
//!
//! Contents:
//! - [`StatePublicationEpoch`] — the global even/odd transaction epoch (checked arithmetic,
//!   hard disarm at the `u64::MAX - 2` boundary, wrap forbidden);
//! - [`TransactionOverlay`] / [`PoolStateMutation`] / [`FamilyWrite`] — the complete
//!   per-transaction mutation set, per-mutation `StatePoint`/transitional point carried NOW
//!   (D5; only precedence ENFORCEMENT defers to WI-07);
//! - [`StateWriterPermit`] (non-cloneable, minted once by `UniverseBuilder::seal()`) and the
//!   [`StateWriteTxn`] witness every post-seal family-store write requires;
//! - [`StateCommandSender`] — the SEND-ONLY off-thread writer handle (queue-full/rejection ⇒
//!   atomic disarm + reconciliation scheduling, never a silent drop);
//! - [`DirtyRouteScratch`] — boot-sized route staging (no `MAX_DIRTY_ROUTES` cap);
//! - [`RouteAuthorityCellTable`] — §3.2 64-byte seqlock cells, observe-only until WI-08;
//! - [`TaintProducerCounters`] — the D1b coherent `{count, max_seq}` snapshot primitive
//!   (writer publishes `max_seq` FIRST via `fetch_max(Release)`, count LAST; reader is
//!   count-first Acquire double-collect) and [`TaintAuthorityRecord`];
//! - [`ReplayIntegrityShared`] — the D2 replay-integrity authority as ONE packed atomic word
//!   (high 32 bits pin generation, low 32 bits sticky fault flags); every authorization surface
//!   down to the final wire closure reads one coherent Acquire snapshot of that word, and repin
//!   advances the generation and clears the superseded generation's flags in a single AcqRel CAS;
//! - [`DarkFrontierTable`] — D1 rule 1's per-origin dark-frontier table (one row per writer
//!   class from T0's generated inventory, each recording the latest committed frontier of its
//!   class while authority is dark) and the all-classes freeze;
//! - [`StatePublisher`] — the sole writer: §3.3 writer protocol 1–8, `poison_pools`, and the
//!   private frontier-dominating `commit_taint_reconciliation` barrier (freeze ALL classes →
//!   per-row dominance/reconcile verify → covering commit → CAS → replay).
//!
//! The loom-instrumented atomics ARE the shipped implementation (cfg-gated aliases below):
//! under `--cfg loom` the same code paths run in the model checker; QEMU/aarch64 runs are
//! evidence, and the native-ARM litmus run stays an ARMED Phase-6 gate regardless.

use core::num::NonZeroU64;
use std::sync::mpsc;
use std::sync::Arc;

use arrayvec::ArrayVec;

use crate::authority::{
    CanonicalCursor, FeedReplayCursor, StateOrderRelation, StatePoint, TransitionalFeedPoint,
};
use crate::builder::Universe;
use crate::ids::{Generation, PoolId, RouteId};
use crate::pool::{PoolKind, PARAM_POISON_HARD};

/// Loom-gated atomic aliases: the tested (loom) atomics ARE the shipped implementation.
#[cfg(loom)]
pub(crate) mod atomic {
    pub(crate) use loom::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
}
/// Native atomics for non-loom builds (identical API surface).
#[cfg(not(loom))]
pub(crate) mod atomic {
    pub(crate) use core::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};
}

use atomic::{fence, AtomicBool, AtomicU64, Ordering};

/// Maximum pool mutations per single transaction overlay (protocol-bounded; exceeding it is
/// typed taint + reconciliation, NEVER a prefix commit). engine-detect's `MAX_DIRTY_POOLS`
/// mirrors this value (single source once WI-06 T4 rewires the alias).
pub const MAX_DIRTY_POOLS: usize = 8;

// =============================================================================================
// Global transaction epoch
// =============================================================================================

/// A committed (even) publication epoch, boot-local (paired with `boot_id` in the journal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedEpoch(pub u64);

/// Approaching this value hard-disarms publication: epoch arithmetic is CHECKED, and wrapping
/// to a previously observed even value (ABA) is forbidden — a controlled restart is required.
pub const EPOCH_HARD_DISARM_BOUNDARY: u64 = u64::MAX - 2;

/// Typed epoch-transition failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochError {
    /// A writer window is already open (odd) — the sole-writer invariant was violated.
    Collision,
    /// The checked-arithmetic hard-disarm boundary was reached; restart required.
    HardDisarm,
}

/// §3.3 `StatePublicationEpoch`: even = stable, odd = writer active. Readers accept only a
/// stable even value observed unchanged across their payload reads.
pub struct StatePublicationEpoch {
    epoch: AtomicU64,
}

impl StatePublicationEpoch {
    /// Boot state: epoch 0, even, stable.
    pub fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    fn with_value(v: u64) -> Self {
        Self {
            epoch: AtomicU64::new(v),
        }
    }

    /// Acquire-load; `Some(even)` only when stable (even, not hard-disarmed).
    pub fn load_stable(&self) -> Option<u64> {
        let e = self.epoch.load(Ordering::Acquire);
        (e & 1 == 0 && e < EPOCH_HARD_DISARM_BOUNDARY).then_some(e)
    }

    /// Raw Acquire load (diagnostics; readers use [`Self::load_stable`]).
    pub fn raw(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Whether the epoch reached the hard-disarm boundary.
    pub fn is_disarmed(&self) -> bool {
        self.epoch.load(Ordering::Acquire) >= EPOCH_HARD_DISARM_BOUNDARY
    }

    /// Open the writer window (even → odd) with CHECKED arithmetic: an already-odd epoch is
    /// a typed `Collision` (the sole-writer invariant was violated — no spin, no silent
    /// success), and a window that would reach the `u64::MAX - 2` boundary hard-disarms
    /// instead of ever wrapping (ABA forbidden).
    fn open_odd(&self) -> Result<u64, EpochError> {
        let cur = self.epoch.load(Ordering::Acquire);
        if cur & 1 != 0 {
            return Err(EpochError::Collision);
        }
        let odd = match cur.checked_add(1) {
            Some(v) if v < EPOCH_HARD_DISARM_BOUNDARY => v,
            _ => {
                self.hard_disarm();
                return Err(EpochError::HardDisarm);
            }
        };
        match self
            .epoch
            .compare_exchange(cur, odd, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                // NIT-1: establish the SeqCst opening barrier (mirroring SeqlockWords::write)
                // AFTER the collision CAS succeeds and BEFORE returning, so a paired reader can
                // never observe a payload store while still seeing the old EVEN generation.
                self.epoch.store(odd, Ordering::SeqCst);
                fence(Ordering::SeqCst);
                Ok(odd)
            }
            Err(_) => Err(EpochError::Collision),
        }
    }

    /// Close the writer window (odd → next even). Release-stores the next even value;
    /// reaching the boundary hard-disarms instead of wrapping.
    fn close_even(&self, odd: u64) -> Result<CommittedEpoch, EpochError> {
        debug_assert_eq!(odd & 1, 1, "close_even takes the open odd value");
        let next = match odd.checked_add(1) {
            Some(v) if v < EPOCH_HARD_DISARM_BOUNDARY => v,
            _ => {
                self.hard_disarm();
                return Err(EpochError::HardDisarm);
            }
        };
        self.epoch.store(next, Ordering::Release);
        Ok(CommittedEpoch(next))
    }

    /// Terminal hard disarm: park the epoch at an odd terminal value forever.
    fn hard_disarm(&self) {
        self.epoch.store(u64::MAX, Ordering::Release);
    }
}

impl Default for StatePublicationEpoch {
    fn default() -> Self {
        Self::new()
    }
}

/// §3.3 reader protocol steps 1/4/5 around a payload read: acquire-load the epoch (odd ⇒
/// inconclusive WITHOUT spinning), run `read`, `fence(Acquire)` (the load barrier ordering
/// the payload loads before the marker re-read), re-load, accept only if identical and even.
/// One immediate retry is the CALLER's policy, outside the inner quote loop.
pub fn epoch_read_pair<R>(
    epoch: &StatePublicationEpoch,
    read: impl FnOnce() -> R,
) -> Option<(u64, R)> {
    let first = epoch.load_stable()?;
    let value = read();
    fence(Ordering::Acquire);
    let second = epoch.epoch.load(Ordering::Acquire);
    (first == second).then_some((first, value))
}

// =============================================================================================
// Mutations, overlay, batches
// =============================================================================================

/// D5: the per-mutation provenance carried in WI-06. The feed path stamps the boot-local
/// transitional point; canonical batches stamp real `StatePoint`s. Precedence ENFORCEMENT
/// (per-writer-class `state_order` rejection rules) is WI-07's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationPoint {
    /// Boot-local non-authoritative feed trigger (D3).
    Transitional(TransitionalFeedPoint),
    /// Full §3.1 point (canonical / exact-RPC / shadow origins).
    State(StatePoint),
}

/// One family-store write payload (prediction-level values; the publisher performs the same
/// slot-preserving merge `commit_prediction` performs today — preserve token maps etc.).
#[derive(Debug, Clone)]
pub enum FamilyWrite {
    /// V2-family reserves.
    V2(crate::v2_state::V2State),
    /// V3-family state (sqrtPrice/tick/liquidity/ticks).
    V3(crate::v3_state::V3State),
    /// Curve StableSwap-NG kernel (slot token map preserved on apply).
    Curve(engine_amm::curve::StableSwapState),
    /// Curve CryptoSwap-NG slot (carries its own token map).
    CurveCrypto(Box<crate::curve_crypto_state::CryptoCurveSlot>),
    /// DODO V2 state (carries its own tokens).
    Dodo(Box<engine_amm::dodo_v2::DodoState>),
    /// Balancer stable kernel (slot token map + poolId preserved on apply).
    BalancerStable(Box<engine_amm::balancer_stable::BalancerStableState>),
    /// Trader Joe LB kernel (slot token0/token1 preserved on apply).
    Lb(Box<engine_amm::trader_joe_lb::LBState>),
    /// Balancer weighted slot (carries its own tokens/weights/scaling).
    BalancerWeighted(Box<crate::balancer_weighted_state::BalancerWeightedSlot>),
    /// Fluid DEX snapshot.
    Fluid(Box<crate::fluid_state::FluidPoolSnapshot>),
    /// Maverick V2 state.
    Maverick(Box<engine_amm::maverick_v2::MaverickState>),
}

/// Sentinel `expected_prior_generation` meaning "skip the F3 stale-over-new check": the producer
/// has no comparable prior generation to validate against (test fixtures; a covering/barrier batch
/// that is ground truth by construction). A real off-thread producer sets the pool generation it
/// observed at compute time so the publisher can refuse a stale full-slot write. `u64::MAX` is
/// unreachable as a real generation (a pool would need 2^64 updates), so it never collides.
pub const UNVALIDATED_PRIOR_GENERATION: Generation = u64::MAX;

/// Sentinel `pre_replay_seq` meaning "no pre-replay-seq guard": every NON-shadow producer sets this,
/// because it stamps `expected_prior_generation` at read time and the F3 generation check is
/// sufficient. Only the SHADOW REVM promoter sets a real value (the global commit sequence it captured
/// BEFORE its replay), because it cannot stamp a per-pool generation pre-replay (pool identity IS the
/// replay output — see [`PoolStateMutation::pre_replay_seq`]). `u64::MAX` is unreachable as a real
/// commit sequence (2^64 commits), so it never collides.
pub const UNVALIDATED_PRE_REPLAY_SEQ: u64 = u64::MAX;

/// One pool mutation inside a transaction overlay / mutation batch.
#[derive(Debug, Clone)]
pub struct PoolStateMutation {
    /// Target pool.
    pub pool: PoolId,
    /// Provenance of THIS mutation (D5 — carried now, enforced at WI-07).
    pub point: MutationPoint,
    /// The pool generation the producer observed when it COMPUTED this full-slot write. F3
    /// (codex item 5): the publisher refuses the write fail-closed (poison + resync) if the pool's
    /// generation has advanced since — applying a full-slot value computed from an older
    /// generation would silently lose the intervening delta (the WSS back-to-back-delta bug).
    /// [`UNVALIDATED_PRIOR_GENERATION`] skips the check. (Also the WI-07 precedence input.)
    pub expected_prior_generation: Generation,
    /// WI-06 T3 fix round 4 (codex round-4 item 5): the GLOBAL commit sequence the SHADOW REVM
    /// promoter captured BEFORE its replay. The shadow cannot stamp a per-pool generation PRE-replay
    /// (a touched pool's identity is a replay OUTPUT — discovered from the replay's changed slots), so
    /// `expected_prior_generation` alone can be DEFEATED: a resident commit landing DURING the replay
    /// is absorbed by the post-replay generation stamp, so the F3 check passes and the stale-fork
    /// full-slot write is BLESSED over the fresher committed value. This field closes that: the
    /// publisher additionally REFUSES the write if the pool's `last_commit_seq` advanced past this
    /// value (the pool committed since before the replay ⇒ the fork is stale ⇒ bless is impossible).
    /// NON-shadow producers set [`UNVALIDATED_PRE_REPLAY_SEQ`] — they stamp `expected_prior_generation`
    /// correctly at read time, so the generation check is sufficient for them. This does NOT weaken
    /// that check; it is an ADDITIONAL fail-closed guard for the shadow's architecturally-late stamp.
    pub pre_replay_seq: u64,
    /// WI-06 T3 fix round 5 (codex round-5 item 5): the SHADOW value-parity (CAS) pre-image — the
    /// pool's FORK PRE-STATE the prediction was computed FROM (for V2: the pre-swap reserves; for the
    /// V3/getter-overlay families: the fork's pre-swap typed state), in resident representation.
    ///
    /// The `pre_replay_seq` guard above cannot close two interleavings codex demonstrated: (a) a commit
    /// captured mid-odd-window whose captured seq already equals the committed seq, and (b) a REVM fork
    /// that simply LAGS a completed commit (its captured seq already equals the committed seq). Both
    /// defeat the strict-`>` seq comparison because the fork read PRE-commit state while the seq looks
    /// current. This field closes them by CAS: the publisher BYTE-COMPARES this pre-image against the
    /// pool's CURRENT resident state (swap-load-bearing fields) BEFORE applying and REFUSES on any
    /// mismatch (resync-only, no poison — the resident is fresher/diverged, not a lost delta). If the
    /// resident advanced at ANY point the fork's pre-image cannot match, so a shadow write lands ONLY
    /// on a resident slot byte-identical to what the fork replayed FROM. `None` for every NON-shadow
    /// producer (they carry no fork pre-image); a `Shadow`-origin write MUST carry `Some` or it is
    /// refused (fail-closed totality — see [`Self`]'s use in `apply_mutation_batch`).
    pub shadow_pre_image: Option<FamilyWrite>,
    /// The family write payload.
    pub write: FamilyWrite,
}

/// §3.3 `TransactionOverlay`: the COMPLETE mutation set of one transaction. Preallocated /
/// recycled; exceeding `MAX_DIRTY_POOLS` marks the overlay overflowed — publication must then
/// refuse the WHOLE overlay (typed taint + reconciliation), never commit a prefix.
#[derive(Debug)]
pub struct TransactionOverlay {
    /// Which writer/origin class produced this overlay (D1 dark-frontier row selector).
    pub origin: BarrierOriginClass,
    /// The transaction-level point (D5).
    pub point: MutationPoint,
    /// Final per-pool writes (at most one per pool).
    pub mutations: ArrayVec<PoolStateMutation, MAX_DIRTY_POOLS>,
    /// Inverse mutations (filled by the publisher during staging; journaled).
    pub inverse: ArrayVec<PoolStateMutation, MAX_DIRTY_POOLS>,
    /// Touched pool set (filled by the publisher during staging).
    pub touched: ArrayVec<PoolId, MAX_DIRTY_POOLS>,
    overflowed: bool,
}

impl TransactionOverlay {
    /// New empty overlay for one transaction.
    pub fn new(origin: BarrierOriginClass, point: MutationPoint) -> Self {
        Self {
            origin,
            point,
            mutations: ArrayVec::new(),
            inverse: ArrayVec::new(),
            touched: ArrayVec::new(),
            overflowed: false,
        }
    }

    /// Stage one mutation. Exceeding `MAX_DIRTY_POOLS` marks the WHOLE overlay overflowed —
    /// publication then refuses it entirely (typed taint + reconciliation), never a prefix.
    pub fn push_mutation(&mut self, m: PoolStateMutation) {
        if self.mutations.try_push(m).is_err() {
            self.overflowed = true;
        }
    }

    /// Whether staging exceeded `MAX_DIRTY_POOLS` (⇒ the whole overlay must be refused).
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Recycle in place for the next transaction (keeps capacity; no allocation).
    pub fn recycle(&mut self, origin: BarrierOriginClass, point: MutationPoint) {
        self.origin = origin;
        self.point = point;
        self.mutations.clear();
        self.inverse.clear();
        self.touched.clear();
        self.overflowed = false;
    }
}

/// One off-thread PARAM-POISON request. Poison is a `PoolMeta` write (`poison_param` +
/// generation bump), not a family-store slot write, so it has no [`FamilyWrite`]
/// representation — an off-thread writer (the WSS param-guard branches, the resync Camelot
/// stable-flip sweep) that must SET poison routes it here so the SOLE publisher applies it
/// with its witness on the drain. `clear_soft_poison` stays a benign multi-writer atomic
/// (CAS SOFT→NONE, never touches HARD) and is NOT routed. `level` is monotonic (`fetch_max`):
/// a HARD is never downgraded by a concurrent SOFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolPoison {
    /// Pool to poison.
    pub pool: PoolId,
    /// `PARAM_POISON_*` level to raise to (monotonic).
    pub level: u8,
}

/// Bounded immutable command an OFF-THREAD writer (WSS log, exact resync, Fluid poll, shadow
/// REVM, mirror, canonical reconciliation, hydration) returns to the sole publisher instead
/// of calling family `set`/`swap` directly.
#[derive(Debug)]
pub struct StateMutationBatch {
    /// Which writer/origin class produced this batch (D1 dark-frontier row selector).
    pub origin: BarrierOriginClass,
    /// Mutations, each carrying its own point (D5).
    pub mutations: ArrayVec<PoolStateMutation, MAX_DIRTY_POOLS>,
    /// Param-poison SETs to apply on the drain (publisher-owned; see [`PoolPoison`]). Applied
    /// AFTER the family writes commit, within the same drain step, so a poisoned pool's
    /// generation bump + DirtyEvent land together (commit-before-wake).
    pub poison: ArrayVec<PoolPoison, MAX_DIRTY_POOLS>,
    /// WI-06 F4 (codex item 8): SOFT-poison CLEARs executed on the drain AFTER this batch's family
    /// writes commit (under the same txn). Because the clear rides WITH the repair write, a
    /// REJECTED batch clears nothing — the pool stays poisoned (fail-closed). The publisher is the
    /// sole executor; no off-thread path clears a poison directly (was the item-8 bypass).
    ///
    /// WI-06 G4 (codex item 8-residual): each entry carries `(pool, poison_version)` — the poison
    /// version the resync VERIFIED the clear against. The publisher clears ONLY if the pool's
    /// current version still matches (a newer poison superseded it ⇒ the clear is a no-op) AND the
    /// batch's repair write for that pool actually applied (a stale-refused repair clears nothing).
    pub clear_soft: ArrayVec<(PoolId, u64), MAX_DIRTY_POOLS>,
}

impl StateMutationBatch {
    /// A batch carrying exactly one family-store mutation (the common off-thread case: one
    /// independent pool = its own single-mutation batch, respecting the overlay cap).
    pub fn single(origin: BarrierOriginClass, mutation: PoolStateMutation) -> Self {
        let mut mutations = ArrayVec::new();
        mutations.push(mutation);
        Self {
            origin,
            mutations,
            poison: ArrayVec::new(),
            clear_soft: ArrayVec::new(),
        }
    }

    /// A batch carrying exactly one param-poison SET (no family write).
    pub fn poison_one(origin: BarrierOriginClass, pool: PoolId, level: u8) -> Self {
        let mut poison = ArrayVec::new();
        poison.push(PoolPoison { pool, level });
        Self {
            origin,
            mutations: ArrayVec::new(),
            poison,
            clear_soft: ArrayVec::new(),
        }
    }

    /// F4: a REPAIR batch — one family-store write PLUS a SOFT-poison clear for `clear_pool`. On
    /// the drain the write commits and THEN the clear runs, both under one txn. If the batch is
    /// rejected NEITHER lands, so the pool stays poisoned (fail-closed): a poison is never cleared
    /// ahead of the repair that justifies it.
    pub fn repair(
        origin: BarrierOriginClass,
        mutation: PoolStateMutation,
        clear_pool: PoolId,
        clear_poison_version: u64,
    ) -> Self {
        let mut mutations = ArrayVec::new();
        mutations.push(mutation);
        let mut clear_soft = ArrayVec::new();
        clear_soft.push((clear_pool, clear_poison_version));
        Self {
            origin,
            mutations,
            poison: ArrayVec::new(),
            clear_soft,
        }
    }

    /// F4: a CLEAR-ONLY batch — a SOFT-poison clear for `pool` with no family write (the repair
    /// verified the resident params already match live, so there is nothing to write; the clear +
    /// its wake are still publisher-owned so they land only on a committed drain). WI-06 G4:
    /// `clear_poison_version` is the version the resync verified against (see [`Self::clear_soft`]).
    pub fn clear_soft_one(origin: BarrierOriginClass, pool: PoolId, clear_poison_version: u64) -> Self {
        let mut clear_soft = ArrayVec::new();
        clear_soft.push((pool, clear_poison_version));
        Self {
            origin,
            mutations: ArrayVec::new(),
            poison: ArrayVec::new(),
            clear_soft,
        }
    }
}

/// Typed rejection of an off-thread batch (bounded queue full / publisher gone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchRejected;

// =============================================================================================
// Universe brand (sole-writer permit ↔ store binding) — F1 / codex item 1
// =============================================================================================

/// A process-unique universe brand stamped into the sole-writer [`StateWriterPermit`], every
/// [`StateWriteTxn`] it opens, every family store, and the pool table. Each witnessed
/// store/pool mutation calls [`StateWriteTxn::assert_universe`] with the store's brand, so a
/// permit minted by a DIFFERENT (e.g. throwaway) builder is UNUSABLE against another universe's
/// resident state — the forgeable-permit class (codex item 1) is closed structurally, not by
/// convention.
///
/// [`UniverseId::WILDCARD`] (value 1) is the TEST/UNBRANDED sentinel: it matches any brand (and
/// any store whose own brand is WILDCARD matches any txn). It is produced ONLY by
/// [`StateWriterPermit::test_mint`] and by standalone `Store::new()`/`with_capacity()` — NEVER
/// by [`next_universe_id`] — so no production permit or sealed store is ever a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniverseId(pub NonZeroU64);

impl UniverseId {
    /// The test/unbranded sentinel (value 1; matches any brand). Never produced by
    /// [`next_universe_id`], which starts at 2.
    pub const WILDCARD: UniverseId = UniverseId(NonZeroU64::MIN);
}

/// Process-wide monotonic source of real (non-wildcard) universe brands; starts at 2 so a real
/// universe brand is never [`UniverseId::WILDCARD`]. Native atomic (NOT the loom-gated alias):
/// it is a plain id source consumed once per builder, not part of the modeled concurrency.
static NEXT_UNIVERSE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(2);

/// Allocate the next process-unique universe brand. Called exactly once per
/// [`crate::builder::UniverseBuilder::new`].
pub(crate) fn next_universe_id() -> UniverseId {
    let raw = NEXT_UNIVERSE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    UniverseId(NonZeroU64::new(raw).expect("universe-id counter exhausted (wrapped to 0)"))
}

// =============================================================================================
// Writer permit + transaction witness
// =============================================================================================

/// THE one non-cloneable sole-writer permit, minted exactly once by
/// `UniverseBuilder::new()`/`seal()` (T3) and BRANDED with that universe's [`UniverseId`].
/// Holding a [`StateWriteTxn`] derived from it is the ONLY way to mutate a family store after
/// sealing, AND that txn only authorizes writes to stores sharing its brand.
pub struct StateWriterPermit {
    universe: UniverseId,
    /// WI-06 round 3 (codex item 1 residual): a retained strong clone of the sealed universe,
    /// attached at [`UniverseBuilder::seal`](crate::builder::UniverseBuilder::seal). Holding it
    /// keeps the sealed universe's `Arc` strong-count > 1 for this permit's whole lifetime, so a
    /// production caller can NEVER `Arc::get_mut`/`Arc::try_unwrap` the shared `Arc<Universe>`
    /// handed out by `into_parts` back into an owned `&mut Universe` to replace a sealed store out
    /// of band. `None` only for the cradle permit (minted before the universe exists) and for test
    /// permits (which never seal a real universe).
    universe_anchor: Option<std::sync::Arc<crate::builder::Universe>>,
}

// Manual `Debug` (not derived): the anchored `Arc<Universe>` is intentionally NOT `Debug`; we only
// report whether an anchor is held.
impl std::fmt::Debug for StateWriterPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateWriterPermit")
            .field("universe", &self.universe)
            .field("universe_anchored", &self.universe_anchor.is_some())
            .finish()
    }
}

impl StateWriterPermit {
    /// Crate-private mint — `UniverseBuilder::new()` is the sole production caller and supplies
    /// the freshly allocated [`next_universe_id`] brand shared with every store of that universe.
    pub(crate) fn mint(universe: UniverseId) -> Self {
        Self {
            universe,
            universe_anchor: None,
        }
    }

    /// WI-06 round 3 (codex item 1 residual): retain a strong clone of the sealed universe for
    /// this permit's lifetime so the shared `Arc<Universe>` can never be recovered as an owned
    /// `&mut Universe`. Called EXACTLY ONCE, by `UniverseBuilder::seal()`, immediately after the
    /// universe is `Arc`-wrapped and before it is handed out via `into_parts`.
    pub(crate) fn attach_universe_anchor(
        &mut self,
        universe: std::sync::Arc<crate::builder::Universe>,
    ) {
        self.universe_anchor = Some(universe);
    }

    /// Test-only mint for substrate/loom/downstream-test writers: mints the WILDCARD brand so a
    /// test can witness a store it did not itself seal. Unreachable in production — the cfg gates
    /// keep it out of every release/production build:
    /// - `test`: engine-state's own `#[cfg(test)]` substrate tests;
    /// - `loom`: the model checker compiles `test_mint` to construct permits INSIDE loom models
    ///   (loom runs are a SEPARATE compilation with `--cfg loom` and no `#[cfg(test)]`, so
    ///   without the `loom` gate the loom substrate tests could not build a permit at all);
    /// - `test-permit` feature: enabled ONLY by downstream crates' dev-dependencies (engine's
    ///   `[dev-dependencies] engine-state` turns it on for `cargo test -p engine`), never by any
    ///   normal `[dependencies]` edge, so a production `cargo build` leaves it off.
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn test_mint() -> Self {
        Self {
            universe: UniverseId::WILDCARD,
            universe_anchor: None,
        }
    }

    /// Open a write transaction witness borrowing this permit (carrying its universe brand).
    pub fn begin_txn(&mut self) -> StateWriteTxn<'_> {
        let universe = self.universe;
        StateWriteTxn {
            _permit: self,
            universe,
            _not_thread: std::marker::PhantomData,
        }
    }
}

/// The write-transaction witness: proof, at the type level, that the holder is the sole
/// publisher (or an inline state-thread writer holding the permit), AND — via its universe
/// brand — that it is authorized for THIS universe's stores. Family-store write entry points
/// require it after sealing (T3 threads the parameter) and assert the brand (F1).
///
/// WI-06 round 3 (codex new `Sync` defect): a witness is SINGLE-THREADED — neither `Send` nor
/// `Sync`. One valid witness therefore cannot be shared by reference into scoped threads (or moved
/// to another thread) to authorize CONCURRENT writers. The snippet below MUST fail to compile:
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// // StateWriteTxn is `!Sync` (a `PhantomData<*mut ()>` marker), so this does not compile.
/// assert_sync::<engine_state::publication::StateWriteTxn<'static>>();
/// ```
#[derive(Debug)]
pub struct StateWriteTxn<'p> {
    _permit: &'p mut StateWriterPermit,
    universe: UniverseId,
    /// WI-06 round 3 (codex new `Sync` defect): make the witness `!Send + !Sync` so it cannot be
    /// shared by reference into scoped threads (or moved to another thread) to authorize CONCURRENT
    /// writers. `*mut ()` is neither `Send` nor `Sync`; `PhantomData` occupies no space and carries
    /// no data. The `const _` guards below prove the property at compile time.
    _not_thread: std::marker::PhantomData<*mut ()>,
}

// WI-06 round 3 (codex new `Sync` defect): compile-time proof that a `StateWriteTxn` witness is
// neither `Send` nor `Sync`. Non-lexical — this is real trait resolution (the same mechanism as
// `static_assertions::assert_not_impl_any!`): if the witness ever regains `Send` or `Sync`, the
// `<StateWriteTxn as Ambiguous..<_>>::some_item` path has two applicable impls and FAILS to
// compile. One valid witness therefore can never authorize a concurrent off-thread writer.
const _: fn() = || {
    trait AmbiguousIfSend<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    #[allow(dead_code)]
    struct Invalid;
    impl<T: ?Sized + Send> AmbiguousIfSend<Invalid> for T {}
    let _ = <StateWriteTxn<'static> as AmbiguousIfSend<_>>::some_item;
};
const _: fn() = || {
    trait AmbiguousIfSync<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    #[allow(dead_code)]
    struct Invalid;
    impl<T: ?Sized + Sync> AmbiguousIfSync<Invalid> for T {}
    let _ = <StateWriteTxn<'static> as AmbiguousIfSync<_>>::some_item;
};

impl StateWriteTxn<'_> {
    /// The universe brand this witness authorizes writes for.
    #[inline]
    pub fn universe(&self) -> UniverseId {
        self.universe
    }

    /// Fail-CLOSED brand check performed by every witnessed family-store / pool-table mutation.
    /// A write is authorized iff this witness's brand matches the store's brand, or either side
    /// is the [`UniverseId::WILDCARD`] test sentinel. A real cross-universe mismatch — a permit
    /// minted by a different builder used against another universe's store — is a FORGED writer:
    /// PANIC (release-mode; the engine stops rather than let an unauthorized writer mutate
    /// resident state out of band). Unreachable on the legitimate path where the sole permit and
    /// its universe's stores share one brand.
    #[inline]
    pub fn assert_universe(&self, store: UniverseId) {
        // Production build (WI-06 F1 totality, codex item 1): an EXACT brand match is required —
        // there is NO wildcard escape. A WILDCARD store (value 1) therefore FAILS CLOSED against
        // any real permit, never open; and because `test_mint` is compiled out, no WILDCARD permit
        // exists either. A forged / cross-universe / swapped-in-WILDCARD writer always panics.
        #[cfg(not(any(test, loom, feature = "test-permit")))]
        let authorized = self.universe == store;
        // Test / loom / downstream-test build: the WILDCARD sentinel matches any brand, so a
        // `test_mint` permit (WILDCARD) or a standalone `Store::new()` (WILDCARD) can witness a
        // store it did not itself seal.
        #[cfg(any(test, loom, feature = "test-permit"))]
        let authorized = self.universe == store
            || self.universe == UniverseId::WILDCARD
            || store == UniverseId::WILDCARD;
        if !authorized {
            panic!(
                "state-write witness universe {} does not match store universe {} — a forged or \
                 cross-universe StateWriteTxn cannot mutate resident state (WI-06 sole-writer)",
                self.universe.0, store.0
            );
        }
    }
}

// =============================================================================================
// Dirty-route scratch (boot-sized)
// =============================================================================================

/// Publisher-owned route staging scratch: allocated ONCE at boot with capacity equal to the
/// sealed route count — no arbitrary `MAX_DIRTY_ROUTES` correctness cap, no allocation after
/// warm-up. Generation-marked so `begin` is O(1).
#[derive(Debug)]
pub struct DirtyRouteScratch {
    marks: Vec<u64>,
    mark_epoch: u64,
    dirty: Vec<RouteId>,
}

impl DirtyRouteScratch {
    /// Boot-sized to the sealed route count.
    pub fn new(route_count: usize) -> Self {
        Self {
            marks: vec![0; route_count],
            mark_epoch: 0,
            dirty: Vec::with_capacity(route_count),
        }
    }

    /// Start staging a new transaction's dirty route set.
    pub fn begin(&mut self) {
        self.mark_epoch += 1;
        self.dirty.clear();
    }

    /// Mark one route dirty (idempotent within a transaction).
    pub fn mark(&mut self, route: RouteId) {
        let idx = route.0 as usize;
        if idx < self.marks.len() && self.marks[idx] != self.mark_epoch {
            self.marks[idx] = self.mark_epoch;
            self.dirty.push(route);
        }
    }

    /// The staged dirty route set.
    pub fn staged(&self) -> &[RouteId] {
        &self.dirty
    }
}

// =============================================================================================
// Seqlock cells (§3.2 route authority table — OBSERVE-ONLY in WI-06 — and the taint record)
// =============================================================================================

/// Phases surfaced to the test/loom probe inside a seqlock write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqlockWritePhase {
    /// The odd marker has been published; payload words not yet written.
    AfterOddMarker,
    /// All payload words written; the closing even marker not yet published.
    BeforeEvenMarker,
}

/// Fixed-width seqlock cell: `seq` marker + `N` payload words. `seq == 0` is DISARMED; the
/// first publication is stable generation 2.
#[derive(Debug)]
struct SeqlockWords<const N: usize> {
    seq: AtomicU64,
    words: [AtomicU64; N],
}

impl<const N: usize> SeqlockWords<N> {
    fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            words: core::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Writer protocol (§3.2, target Graviton memory model): `seq.store(odd, SeqCst)`;
    /// `fence(SeqCst)`; N Relaxed payload stores; `fence(Release)`;
    /// `seq.store(next_even, Release)`. The initial SeqCst marker/fence forbids any payload
    /// word from becoming visible before readers can observe odd. Returns the new even
    /// generation.
    fn write(&self, values: &[u64; N]) -> u64 {
        self.write_with_probe(values, |_| {})
    }

    fn write_with_probe(&self, values: &[u64; N], mut probe: impl FnMut(SeqlockWritePhase)) -> u64 {
        let cur = self.seq.load(Ordering::Relaxed);
        let odd = cur | 1;
        self.seq.store(odd, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        probe(SeqlockWritePhase::AfterOddMarker);
        for (w, v) in self.words.iter().zip(values.iter()) {
            w.store(*v, Ordering::Relaxed);
        }
        probe(SeqlockWritePhase::BeforeEvenMarker);
        fence(Ordering::Release);
        let next_even = odd.wrapping_add(1);
        self.seq.store(next_even, Ordering::Release);
        next_even
    }

    /// Reader protocol: `seq.load(Acquire)` (require the EXPECTED nonzero even value);
    /// N Relaxed payload loads; `fence(Acquire)` (the load barrier ordering the payload loads
    /// before the marker re-read); `seq.load(Acquire)`; accept iff unchanged.
    fn read(&self, expected_even: u64) -> Option<[u64; N]> {
        if expected_even == 0 || expected_even & 1 != 0 {
            return None;
        }
        let first = self.seq.load(Ordering::Acquire);
        if first != expected_even {
            return None;
        }
        let mut out = [0u64; N];
        for (o, w) in out.iter_mut().zip(self.words.iter()) {
            *o = w.load(Ordering::Relaxed);
        }
        fence(Ordering::Acquire);
        let second = self.seq.load(Ordering::Acquire);
        (second == first).then_some(out)
    }

    /// Current marker value (even = a readable generation; odd = writer mid-flight).
    fn generation(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
}

/// The seven payload words of one §3.2 route-authority cell (the route id is the table
/// index; the compact token layout is bound at WI-08 activation — observe-only here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteAuthorityWords(pub [u64; 7]);

/// 64-byte-aligned cell: `seq` + seven payload words = exactly 64 bytes.
#[repr(align(64))]
#[derive(Debug)]
struct RouteAuthorityCell(SeqlockWords<7>);

/// §3.2 boot-preallocated route-authority cell table. OBSERVE-ONLY in WI-06: the publisher
/// stamps cells inside its odd window; NOTHING consumes them for wire authority until WI-08
/// replaces the zero-pin gate (armed by `wi08_route_cells_observe_only_tripwire_is_armed`).
#[derive(Debug)]
pub struct RouteAuthorityCellTable {
    cells: Box<[RouteAuthorityCell]>,
}

impl RouteAuthorityCellTable {
    /// Boot-sized to the sealed route count; every cell starts DISARMED (`seq == 0`).
    pub fn new(route_count: usize) -> Self {
        let mut cells = Vec::with_capacity(route_count);
        for _ in 0..route_count {
            cells.push(RouteAuthorityCell(SeqlockWords::new()));
        }
        Self {
            cells: cells.into_boxed_slice(),
        }
    }

    /// Sole-writer cell stamp (witnessed). Returns the new even generation.
    pub fn write_route_cell(
        &self,
        _txn: &StateWriteTxn<'_>,
        route: RouteId,
        words: RouteAuthorityWords,
    ) -> Option<u64> {
        self.cells
            .get(route.0 as usize)
            .map(|c| c.0.write(&words.0))
    }

    #[cfg(any(test, loom))]
    fn write_route_cell_with_probe(
        &self,
        route: RouteId,
        words: RouteAuthorityWords,
        probe: impl FnMut(SeqlockWritePhase),
    ) -> Option<u64> {
        self.cells
            .get(route.0 as usize)
            .map(|c| c.0.write_with_probe(&words.0, probe))
    }

    /// §3.2 reader: both marker reads must equal `expected_even_generation` around the
    /// payload loads. `None` = disarmed / mid-write / generation moved — the caller declines.
    pub fn read_route_cell(
        &self,
        route: RouteId,
        expected_even_generation: u64,
    ) -> Option<RouteAuthorityWords> {
        self.cells
            .get(route.0 as usize)
            .and_then(|c| c.0.read(expected_even_generation))
            .map(RouteAuthorityWords)
    }

    /// Current cell generation (even = readable, odd = writer mid-flight, 0 = disarmed).
    pub fn cell_generation(&self, route: RouteId) -> Option<u64> {
        self.cells.get(route.0 as usize).map(|c| c.0.generation())
    }
}

// =============================================================================================
// D1b — coherent producer taint snapshot + symmetric live-counter revocation
// =============================================================================================

/// A coherent `{content, terminal, max_seq}` producer-counter snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaintCounterSnapshot {
    /// Re-armable content-taint count.
    pub content: u64,
    /// Terminal-class taint count.
    pub terminal: u64,
    /// Upper bound on the sequence of every COUNTED taint.
    pub max_seq: u64,
}

/// D1b producer counter block. Writer order at EVERY producer site: publish the sequence
/// bound FIRST (`max_seq.fetch_max(seq, Release)`), count LAST (`fetch_add(1, Release)`) — a
/// taint is "counted" only after its sequence is already in the bound. Reader: count-first
/// Acquire double-collect (`c1 → max → c2`, accept iff `c1 == c2`), so an accepted
/// snapshot's bound provably covers every counted taint.
#[derive(Debug)]
pub struct TaintProducerCounters {
    content: AtomicU64,
    terminal: AtomicU64,
    max_seq: AtomicU64,
}

impl TaintProducerCounters {
    /// Zeroed counters.
    pub fn new() -> Self {
        Self {
            content: AtomicU64::new(0),
            terminal: AtomicU64::new(0),
            max_seq: AtomicU64::new(0),
        }
    }

    /// Record one re-armable CONTENT taint at feed sequence `seq`. Writer order (D1b): the
    /// sequence bound is published FIRST (`fetch_max(Release)`), the count LAST
    /// (`fetch_add(Release)`) — a taint is "counted" only after its sequence is already in
    /// the bound, so an accepted count-first double-collect snapshot provably covers it.
    pub fn record_content_taint(&self, seq: u64) {
        self.max_seq.fetch_max(seq, Ordering::Release);
        self.content.fetch_add(1, Ordering::Release);
    }

    /// Record one TERMINAL-class taint at feed sequence `seq` (same writer order).
    pub fn record_terminal_taint(&self, seq: u64) {
        self.max_seq.fetch_max(seq, Ordering::Release);
        self.terminal.fetch_add(1, Ordering::Release);
    }

    /// Test seam: run `between` between the two halves of the content-taint writer, in the
    /// SHIPPED order — the deterministic schedule for the D1b writer-order discriminator
    /// (max-first: the paused state shows the bound already covering the yet-uncounted
    /// taint; the reversed-order mutant shows a counted taint outside the bound).
    #[cfg(any(test, loom))]
    pub fn record_content_taint_split_for_test(&self, seq: u64, between: impl FnOnce()) {
        self.max_seq.fetch_max(seq, Ordering::Release);
        between();
        self.content.fetch_add(1, Ordering::Release);
    }

    /// Recorded REVERSED-order mutant (test-only discriminator; never production): counts
    /// FIRST — the exact writer-order bug the D1b RED distinguishes.
    #[cfg(any(test, loom))]
    pub fn record_content_taint_reversed_order_mutant(&self, seq: u64, between: impl FnOnce()) {
        self.content.fetch_add(1, Ordering::Release);
        between();
        self.max_seq.fetch_max(seq, Ordering::Release);
    }

    /// Coherent snapshot: count-first Acquire DOUBLE-COLLECT — load counts, load the bound,
    /// re-load the counts; accept only if unchanged (else retry). Every taint counted by an
    /// accepted snapshot published its sequence before incrementing, and in-flight taints
    /// can only make the bound larger (safe over-coverage).
    pub fn snapshot(&self) -> TaintCounterSnapshot {
        loop {
            let c1 = self.content.load(Ordering::Acquire);
            let t1 = self.terminal.load(Ordering::Acquire);
            let max_seq = self.max_seq.load(Ordering::Acquire);
            let c2 = self.content.load(Ordering::Acquire);
            let t2 = self.terminal.load(Ordering::Acquire);
            if c1 == c2 && t1 == t2 {
                return TaintCounterSnapshot {
                    content: c1,
                    terminal: t1,
                    max_seq,
                };
            }
            core::hint::spin_loop();
        }
    }

    /// D1b symmetric live check (two Acquire loads + compares): are the LIVE counters still
    /// exactly the reconciled snapshot? Consulted at permit capture, every final wire write,
    /// every state-authority/route-cell read, and publisher stamping (T5/T7 wiring).
    pub fn current_matches(&self, reconciled: &TaintCounterSnapshot) -> bool {
        self.content.load(Ordering::Acquire) == reconciled.content
            && self.terminal.load(Ordering::Acquire) == reconciled.terminal
    }
}

impl Default for TaintProducerCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// D1 taint authority record: a seqlock cell storing the RECONCILED snapshot
/// `{content, terminal, max_seq, rearm_epoch}` the current armed authority is based on.
#[derive(Debug)]
pub struct TaintAuthorityRecord {
    cell: SeqlockWords<4>,
}

/// The decoded payload of [`TaintAuthorityRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaintAuthorityView {
    /// The reconciled snapshot.
    pub snapshot: TaintCounterSnapshot,
    /// The even epoch at which the re-arm committed.
    pub rearm_epoch: u64,
}

impl TaintAuthorityRecord {
    /// Boot state: disarmed cell (generation 0) — no reconciliation has happened.
    pub fn new() -> Self {
        Self {
            cell: SeqlockWords::new(),
        }
    }

    /// Sole-writer CAS-equivalent stamp inside the barrier (witnessed). Returns the new even
    /// cell generation.
    pub fn stamp(
        &self,
        _txn: &StateWriteTxn<'_>,
        view: TaintAuthorityView,
    ) -> u64 {
        self.cell.write(&[
            view.snapshot.content,
            view.snapshot.terminal,
            view.snapshot.max_seq,
            view.rearm_epoch,
        ])
    }

    /// Seqlock read at an expected even generation.
    pub fn read(&self, expected_even_generation: u64) -> Option<TaintAuthorityView> {
        self.cell.read(expected_even_generation).map(|w| TaintAuthorityView {
            snapshot: TaintCounterSnapshot {
                content: w[0],
                terminal: w[1],
                max_seq: w[2],
            },
            rearm_epoch: w[3],
        })
    }

    /// Current cell generation.
    pub fn generation(&self) -> u64 {
        self.cell.generation()
    }
}

impl Default for TaintAuthorityRecord {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================================
// D2 — shared replay/stream-integrity fault block
// =============================================================================================

/// Poisoned: a hole-class replay fault occurred — the WHOLE pin is a non-prefix.
pub const REPLAY_FLAG_POISONED: u64 = 1 << 0;
/// Stream disorder detected (late-NEW lower/equal cursor at ingress, or drain-side order).
pub const REPLAY_FLAG_DISORDER: u64 = 1 << 1;
/// Terminal conflict (equal cursor, different identity).
pub const REPLAY_FLAG_CONFLICT: u64 = 1 << 2;

/// High 32 bits of the packed replay-integrity word hold the pin generation.
const REPLAY_PIN_GENERATION_SHIFT: u32 = 32;
/// Low 32 bits hold the sticky fault flags; the three flag constants above live here.
const REPLAY_FLAGS_MASK: u64 = u32::MAX as u64;

#[inline]
const fn pack_replay_integrity(pin_generation: u32, flags: u32) -> u64 {
    ((pin_generation as u64) << REPLAY_PIN_GENERATION_SHIFT) | flags as u64
}

/// D2 replay-integrity authority as one linearizable word.
///
/// High 32 bits: pin generation. Low 32 bits: sticky fault flags. Fault publication
/// is a low-word Release RMW; repin is an AcqRel CAS that advances the generation and
/// clears the superseded generation's flags in the same atomic transition. Every
/// authorization decision consumes one Acquire snapshot of this word.
#[derive(Debug)]
pub struct ReplayIntegrityShared {
    packed: AtomicU64,
}

impl ReplayIntegrityShared {
    /// Boot: pin generation 1, flags clear.
    pub fn new() -> Self {
        Self {
            packed: AtomicU64::new(pack_replay_integrity(1, 0)),
        }
    }

    /// One coherent Acquire snapshot: generation equal and every low flag clear.
    #[inline]
    pub fn replay_integrity_current(&self, expected_generation: u64) -> bool {
        let packed = self.packed.load(Ordering::Acquire);
        let generation = packed >> REPLAY_PIN_GENERATION_SHIFT;
        let flags = packed & REPLAY_FLAGS_MASK;
        generation == expected_generation && flags == 0
    }

    /// Live generation from one coherent packed snapshot.
    #[inline]
    pub fn current_pin_generation(&self) -> u64 {
        self.packed.load(Ordering::Acquire) >> REPLAY_PIN_GENERATION_SHIFT
    }

    /// WI-06 D2 B0: capture the CLEAN pin generation for a candidate about to be admitted to the
    /// express lane. One coherent Acquire snapshot of the SAME packed word — `Some(generation)`
    /// only when every fault flag is clear, `None` on any fault. This is a strict subset of
    /// [`Self::replay_integrity_current`] (identical Acquire load; it just returns the generation
    /// when clean), so it adds no second authority atomic and no new ordering. The returned `u64`
    /// is an OWNED value that crosses the sim→submit SPSC as part of one ring item; the actual
    /// authorization is always re-evaluated against the live word at express-pop and at the
    /// unchanged final wire gate, so a stale capture can only be REFUSED — never a false accept.
    #[inline]
    pub fn capture_clean_pin_generation(&self) -> Option<u64> {
        let packed = self.packed.load(Ordering::Acquire);
        let generation = packed >> REPLAY_PIN_GENERATION_SHIFT;
        let flags = packed & REPLAY_FLAGS_MASK;
        (flags == 0).then_some(generation)
    }

    /// Publish a low-word fault without modifying generation.
    pub fn publish_fault(&self, flag: u64) {
        assert!(
            flag != 0 && flag & !REPLAY_FLAGS_MASK == 0,
            "replay-integrity fault flag must be non-zero and confined to the low 32 bits"
        );
        self.packed.fetch_or(flag, Ordering::Release);
    }

    /// Whether any low-word fault flag is set (generation-independent).
    #[inline]
    pub fn faulted(&self) -> bool {
        self.packed.load(Ordering::Acquire) & REPLAY_FLAGS_MASK != 0
    }

    /// Atomically advance generation and clear every prior-generation flag.
    pub fn repin_clear(&self) -> u64 {
        let mut observed = self.packed.load(Ordering::Acquire);
        loop {
            let current_generation =
                (observed >> REPLAY_PIN_GENERATION_SHIFT) as u32;
            let next_generation = current_generation.checked_add(1).expect(
                "replay-integrity pin generation exhausted; restart required before wrap",
            );
            let next = pack_replay_integrity(next_generation, 0);
            match self.packed.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return u64::from(next_generation),
                Err(actual) => observed = actual,
            }
        }
    }
}

impl Default for ReplayIntegrityShared {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================================
// D1 — per-origin dark-frontier table + barrier
// =============================================================================================

/// The writer/origin classes of T0's GENERATED inventory — one dark-frontier row each.
/// `wi06_sole_writer_static.rs` derives the same set from source; T7's
/// `barrier_completion_predicate_covers_the_generated_writer_inventory` pins equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BarrierOriginClass {
    /// state_loop feed prediction commits (incl. sniff).
    FeedTrigger = 0,
    /// Receipt-log replay mutations.
    Receipt = 1,
    /// WSS log-subscription applier mutations.
    WssLog = 2,
    /// Exact resync mutations.
    Resync = 3,
    /// Fluid poll-cycle mutations.
    Fluid = 4,
    /// Mirror-derived mutations.
    Mirror = 5,
    /// Shadow-REVM post-state promotions.
    Shadow = 6,
}

impl BarrierOriginClass {
    /// Every row, in table order.
    pub const ALL: [Self; 7] = [
        Self::FeedTrigger,
        Self::Receipt,
        Self::WssLog,
        Self::Resync,
        Self::Fluid,
        Self::Mirror,
        Self::Shadow,
    ];

    /// D1 rule-3 dominance class of this row at the re-arm CAS.
    pub fn dominance(self) -> DominanceClass {
        match self {
            Self::FeedTrigger => DominanceClass::Feed,
            Self::Receipt | Self::WssLog | Self::Resync => DominanceClass::CanonicalComparable,
            Self::Fluid | Self::Mirror | Self::Shadow => DominanceClass::ReconcileResetOrSuspend,
        }
    }

    /// Stable row name (ledger/report surface).
    pub fn name(self) -> &'static str {
        match self {
            Self::FeedTrigger => "FeedTrigger",
            Self::Receipt => "Receipt",
            Self::WssLog => "WssLog",
            Self::Resync => "Resync",
            Self::Fluid => "Fluid",
            Self::Mirror => "Mirror",
            Self::Shadow => "Shadow",
        }
    }
}

/// D1 rule-3 dominance class verified PER ROW at the re-arm CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DominanceClass {
    /// anchor block ≥ block(frozen feed frontier).
    Feed,
    /// anchor must DOMINATE the row's latest committed canonical cursor (`state_order`
    /// lineage comparison, never bare height).
    CanonicalComparable,
    /// Dominance undefined: the row must be reconciled at the anchor, reset, or its trust
    /// suspended before the CAS may proceed.
    ReconcileResetOrSuspend,
}

/// The latest committed frontier of one origin class while authority is dark (the rev-4
/// TEMPORAL binding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierRecord {
    /// No commit of this class occurred while dark.
    Absent,
    /// Latest committed feed replay cursor.
    Feed(FeedReplayCursor),
    /// Latest committed canonical cursor.
    Canonical(CanonicalCursor),
    /// Incomparable identity: the row's opaque per-commit identity generation.
    Opaque {
        /// Monotonic per-commit identity generation.
        identity_generation: u64,
    },
}

/// D1 rule 1: the per-origin dark-frontier TABLE — one row per writer/origin class, each
/// recording the LATEST COMMITTED point/generation of that class, updated at every dark
/// commit. The re-arm CAS validates the COMPLETE frozen table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarkFrontierTable {
    rows: [FrontierRecord; 7],
}

impl DarkFrontierTable {
    /// All rows absent (dark just began).
    pub fn new() -> Self {
        Self {
            rows: [FrontierRecord::Absent; 7],
        }
    }

    /// Record a dark commit of `class` at `record` (keeps the newest per class).
    pub fn record_commit(&mut self, class: BarrierOriginClass, record: FrontierRecord) {
        let row = &mut self.rows[class as usize];
        *row = match (&row, &record) {
            (FrontierRecord::Feed(cur), FrontierRecord::Feed(new)) if new <= cur => *row,
            (
                FrontierRecord::Canonical(cur),
                FrontierRecord::Canonical(new),
            ) if new.block_number <= cur.block_number => *row,
            _ => record,
        };
    }

    /// The row for `class`.
    pub fn row(&self, class: BarrierOriginClass) -> FrontierRecord {
        self.rows[class as usize]
    }

    /// Row count (pinned == inventory row count by T7's predicate test).
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

impl Default for DarkFrontierTable {
    fn default() -> Self {
        Self::new()
    }
}

/// How an INCOMPARABLE row (Fluid / Mirror / Shadow / transitional) was discharged before
/// the re-arm CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomparableTreatment {
    /// Re-fetched/pinned to the anchor's block hash.
    ReconciledAtAnchor,
    /// Reset (state discarded; rebuilt post-re-arm).
    Reset,
    /// Trust suspended: marked non-authoritative past re-arm.
    TrustSuspended,
}

/// Fork/mirror authority-surface obligation state at the barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceObligation {
    /// Repinned/re-anchored at or above the anchor.
    RepinnedAtOrAboveAnchor,
    /// Reset.
    Reset,
    /// Trust suspended.
    Suspended,
    /// NOT discharged — the barrier is incomplete.
    Unmet,
}

/// A candidate completed taint barrier presented to the re-arm CAS.
#[derive(Debug)]
pub struct CompletedTaintBarrier {
    /// The canonical anchor the covering batch was built against.
    pub anchor: CanonicalCursor,
    /// The producer-counter snapshot the barrier reconciled.
    pub snapshot: TaintCounterSnapshot,
    /// The covering batch itself (committed at a stable even epoch by the barrier).
    pub covering_batch: StateMutationBatch,
    /// Per-incomparable-row treatments (indexed by `BarrierOriginClass as usize`; rows whose
    /// dominance class is comparable ignore their entry).
    pub treatments: [Option<IncomparableTreatment>; 7],
    /// Validator-fork obligation.
    pub fork: SurfaceObligation,
    /// Mirror obligation.
    pub mirror: SurfaceObligation,
    /// Feed cursor ↦ block conversion offset (`seq + offset = block`; the caller supplies
    /// the Nitro genesis offset — sequence arithmetic never invents a block INSIDE the
    /// vocabulary, the conversion is the caller's stated contract).
    pub feed_block_offset: u64,
}

/// Outcome of the re-arm CAS.
#[derive(Debug, PartialEq, Eq)]
pub enum RearmOutcome {
    /// Re-armed at this committed even epoch.
    Rearmed(CommittedEpoch),
    /// Refused — the barrier is INCOMPLETE; extend and retry.
    Refused(RearmRefusal),
}

/// Why a re-arm was refused (barrier incomplete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RearmRefusal {
    /// Not dark — nothing to re-arm.
    NotDark,
    /// The anchor does not dominate the frozen FEED frontier row.
    AnchorBehindFeedFrontier,
    /// The anchor does not DOMINATE this canonical-comparable row's latest committed cursor.
    AnchorDoesNotDominateCanonicalRow(BarrierOriginClass),
    /// This incomparable row was neither reconciled, reset, nor suspended.
    IncomparableRowUntreated(BarrierOriginClass),
    /// The live producer counters advanced past the barrier's snapshot.
    TaintCountAdvanced,
    /// The validator-fork obligation is unmet.
    ForkObligationUnmet,
    /// The mirror obligation is unmet.
    MirrorObligationUnmet,
}

// =============================================================================================
// Shared publication authority + off-thread command channel
// =============================================================================================

/// Cap on recorded lost-corrector obligations (each of the [`LostCorrectors`] vectors is bounded to
/// this many DISTINCT ids). A lost corrector is never DROPPED silently below this many outstanding.
///
/// WI-06 round 4 I5c (codex round-4 nit): AT the cap the recording falls CLOSED rather than silently
/// dropping a distinct id — [`PublicationAuthority::note_lost_corrector_saturation`] DISARMS
/// publication authority (the coarse fallback latch) and fires a loud one-shot warning. So the
/// per-pool exact-tracking guarantee holds up to `LOST_CORRECTOR_CAP` distinct outstanding ids, and
/// beyond it the system is in the honest disarmed posture (never a silent drop while armed).
const LOST_CORRECTOR_CAP: usize = 4096;

/// WI-06 round 4 I5c: one-shot latch so the cap-saturation warning is LOUD but not spammed.
/// This is a process-global latch, NEVER part of any loom model, so it uses the real `std`
/// atomic (fully-qualified) exactly like `NEXT_UNIVERSE_ID` above — the loom-aliased `AtomicBool`
/// is not const-constructible and cannot back a `static`.
static LOST_CORRECTOR_CAP_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// F4 (codex item 4): the concrete obligations of a REJECTED off-thread batch — its poison intents
/// (must be re-applied so a lost poison never leaves a pool live-unpoisoned) and its touched pools
/// (must be resynced so a lost write is caught up). Drained by the permit-holding state thread.
#[derive(Debug, Default)]
struct LostCorrectors {
    poison: Vec<PoolPoison>,
    resync: Vec<PoolId>,
    /// WI-06 round 3 H3(b) (codex items 6+11): committed pools whose WAKE was shed (a full
    /// dirty-ring + full wake-spillover on the drain, or the receipt boundary's full-ring fallback).
    /// Their STATE is already committed — only the wake is owed — so a resync of an already-committed
    /// pool writes nothing and would NEVER re-wake it. The state loop drains this set and re-emits a
    /// wake for each (EVEN IF the snapshot is byte-equal), so a committed pool is never left unwoken.
    owed_wake: Vec<PoolId>,
    /// WI-06 G3(a) (codex item 4-residual): reconciliation obligations that carry NO specific
    /// recorded pool (e.g. a wake-spill disarm fallback). Held under the SAME lock as the vectors
    /// so [`PublicationAuthority::take_obligations`] drains poison + resync + count ATOMICALLY —
    /// there is no window in which a raced counter reset strands a concurrently-recorded vector.
    pending: u64,
}

/// Cross-thread publication-authority flags (consulted by T5's reader wiring).
#[derive(Debug)]
pub struct PublicationAuthority {
    disarmed: AtomicBool,
    /// F4 (codex item 4): obligations of rejected/refused batches (poison intents, pools to resync)
    /// AND the bare reconciliation count — all under one lock so a single take is atomic. Recorded
    /// only on the rare rejection/refusal path (off the hot success path).
    lost: std::sync::Mutex<LostCorrectors>,
}

impl PublicationAuthority {
    /// Armed, nothing pending.
    pub fn new() -> Self {
        Self {
            disarmed: AtomicBool::new(false),
            lost: std::sync::Mutex::new(LostCorrectors::default()),
        }
    }

    /// Atomically disarm publication authority.
    pub fn disarm(&self) {
        self.disarmed.store(true, Ordering::Release);
    }

    /// Re-arm (publisher-only; called from the barrier CAS).
    fn rearm(&self) {
        self.disarmed.store(false, Ordering::Release);
    }

    /// Whether authority is disarmed.
    pub fn is_disarmed(&self) -> bool {
        self.disarmed.load(Ordering::Acquire)
    }

    /// Schedule a reconciliation obligation (never silently dropped). Under the obligation lock so
    /// it is atomic with any vector take.
    pub fn schedule_reconciliation(&self) {
        if let Ok(mut lost) = self.lost.lock() {
            lost.pending = lost.pending.saturating_add(1);
        }
    }

    /// Outstanding reconciliation obligations (the bare count; recorded pools are separate).
    pub fn pending_reconciliations(&self) -> u64 {
        self.lost.lock().map(|l| l.pending).unwrap_or(0)
    }

    /// F4: consume the outstanding reconciliation count (read-and-reset), returning how many were
    /// pending. (Retained for observability/tests; [`Self::take_obligations`] resets it as part of
    /// the atomic drain.)
    pub fn take_pending_reconciliations(&self) -> u64 {
        self.lost
            .lock()
            .map(|mut l| std::mem::replace(&mut l.pending, 0))
            .unwrap_or(0)
    }

    /// WI-06 round 4 I5c (codex round-4 nit): a DISTINCT lost-corrector id could not be recorded
    /// because its vector reached [`LOST_CORRECTOR_CAP`]. Rather than SILENTLY drop it — which would
    /// make the "no corrector is ever dropped" / "every committed pool is owed a wake" guarantees
    /// untrue under sustained saturation while authority stayed ARMED — fall CLOSED: DISARM publication
    /// authority (the coarse fallback; the permit-holding state thread will not silently resume
    /// trusting the frontier) and fire a LOUD one-shot warning. The disarm is the honest posture:
    /// precise per-pool tracking was exceeded, so the system drops to conservative rather than
    /// pretending it recorded everything. Called only on the rare saturation path (never hot).
    fn note_lost_corrector_saturation(&self) {
        self.disarm();
        if !LOST_CORRECTOR_CAP_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            // engine-state carries no logging framework; a rare fail-closed saturation warrants a loud
            // stderr line (one-shot). The DISARM above is the machine-observable fail-closed signal.
            eprintln!(
                "WI-06 WARN: lost-corrector tracking SATURATED at cap {LOST_CORRECTOR_CAP} — \
                 publication authority DISARMED (fail-closed); distinct obligations beyond the cap are \
                 no longer individually recorded until a controlled re-arm"
            );
        }
    }

    /// F4: record the obligations of a REJECTED batch (its poison intents + touched pools) so the
    /// permit-holding state thread can re-apply them. Bounded by [`LOST_CORRECTOR_CAP`]; a distinct id
    /// that cannot fit DISARMS authority (fail-closed) via [`Self::note_lost_corrector_saturation`].
    pub(crate) fn record_lost_corrector(&self, batch: &StateMutationBatch) {
        let mut saturated = false;
        if let Ok(mut lost) = self.lost.lock() {
            for pp in &batch.poison {
                if lost.poison.len() < LOST_CORRECTOR_CAP {
                    lost.poison.push(*pp);
                } else {
                    saturated = true; // a lost poison intent could not be recorded — fall closed
                }
            }
            // A lost family write OR a lost repair (clear) needs the pool RESYNCED so it is caught
            // up / re-verified — never left silently with the missing update.
            for pool in batch
                .mutations
                .iter()
                .map(|m| m.pool)
                .chain(batch.clear_soft.iter().map(|(pool, _ver)| *pool))
            {
                if lost.resync.contains(&pool) {
                    continue; // already tracked — deduped, not a drop
                }
                if lost.resync.len() < LOST_CORRECTOR_CAP {
                    lost.resync.push(pool);
                } else {
                    saturated = true; // a distinct lost-write pool could not be recorded — fall closed
                }
            }
        }
        if saturated {
            self.note_lost_corrector_saturation();
        }
    }

    /// WI-06 G3(b/c/e) (codex item 4-residual / item 6): record a SINGLE pool that must be resynced
    /// — a stale-over-new refusal (the poisoned pool a resync must re-snapshot), a targeted-resync
    /// id that could not be enqueued (full queue), or a committed pool whose wake was shed on a
    /// full ring+spillover. Bounded and deduped like `record_lost_corrector`. `pub` so the engine
    /// state loop (holding only the send-only handle's authority) can record these obligations.
    pub fn record_stale_resync(&self, pool: PoolId) {
        let saturated = if let Ok(mut lost) = self.lost.lock() {
            if lost.resync.contains(&pool) {
                false // already tracked — deduped, not a drop
            } else if lost.resync.len() < LOST_CORRECTOR_CAP {
                lost.resync.push(pool);
                false
            } else {
                true // a distinct pool could not be recorded — fall closed (never a silent drop)
            }
        } else {
            false
        };
        if saturated {
            self.note_lost_corrector_saturation();
        }
    }

    /// WI-06 round 3 H3(b) (codex items 6+11): record a committed pool whose WAKE was shed (full
    /// dirty-ring + full spillover on the drain, or the receipt boundary's full-ring fallback). The
    /// state loop drains these via [`take_owed_wakes`](Self::take_owed_wakes) and re-emits a wake for
    /// each — the state is ALREADY committed, so this guarantees the wake EVEN IF a resync snapshot
    /// would be byte-equal (a resync of committed state writes nothing and would never re-wake it).
    /// Bounded + deduped like [`record_stale_resync`](Self::record_stale_resync). `pub` so the engine
    /// state loop / on-thread receipt replayer (holding only the send-only authority) can record it.
    pub fn record_owed_wake(&self, pool: PoolId) {
        let saturated = if let Ok(mut lost) = self.lost.lock() {
            if lost.owed_wake.contains(&pool) {
                false // already owed — deduped, not a drop
            } else if lost.owed_wake.len() < LOST_CORRECTOR_CAP {
                lost.owed_wake.push(pool);
                false
            } else {
                true // a distinct committed pool's owed wake could not be recorded — fall closed
            }
        } else {
            false
        };
        if saturated {
            self.note_lost_corrector_saturation();
        }
    }

    /// WI-06 round 3 H3(b): drain the owed-wake set (committed pools awaiting a re-emitted wake).
    /// Independent of [`take_obligations`](Self::take_obligations) (owed wakes need only a wake, not
    /// the poison/resync coordination), under the SAME lock. Empty when none are owed.
    pub fn take_owed_wakes(&self) -> Vec<PoolId> {
        self.lost
            .lock()
            .map(|mut l| std::mem::take(&mut l.owed_wake))
            .unwrap_or_default()
    }

    /// WI-06 G3(a) (codex item 4-residual): ATOMICALLY drain ALL recorded obligations — poison
    /// intents to re-apply, pools to resync, and the bare reconciliation count — under ONE lock.
    /// Because the count and the vectors are taken together, a concurrent record/schedule can never
    /// be half-consumed (no stranded work). Empty when nothing is pending.
    pub(crate) fn take_obligations(&self) -> (Vec<PoolPoison>, Vec<PoolId>, u64) {
        match self.lost.lock() {
            Ok(mut lost) => (
                std::mem::take(&mut lost.poison),
                std::mem::take(&mut lost.resync),
                std::mem::replace(&mut lost.pending, 0),
            ),
            Err(_) => (Vec::new(), Vec::new(), 0),
        }
    }
}

impl Default for PublicationAuthority {
    fn default() -> Self {
        Self::new()
    }
}

/// SEND-ONLY off-thread writer handle: exposes `submit_batch` and NOTHING else (no publisher,
/// no permit). Queue-full / rejection is a LOST CANONICAL CORRECTOR — authority cannot be
/// maintained — so it must atomically disarm and schedule reconciliation, never silently drop.
#[derive(Debug, Clone)]
pub struct StateCommandSender {
    tx: mpsc::SyncSender<StateMutationBatch>,
    authority: Arc<PublicationAuthority>,
}

impl StateCommandSender {
    /// Submit one bounded batch to the sole publisher. A full/disconnected queue is a LOST
    /// canonical corrector: publication authority disarms ATOMICALLY at the rejection and a
    /// reconciliation obligation is scheduled — never a silent drop, never an ignorable
    /// error.
    pub fn submit_batch(&self, b: StateMutationBatch) -> Result<(), BatchRejected> {
        match self.tx.try_send(b) {
            Ok(()) => Ok(()),
            Err(e) => {
                // F4 (codex item 4): the rejected batch is handed back by `try_send` — record its
                // obligations (poison intents + touched pools) BEFORE dropping it so the state
                // thread can re-apply them; the disarm + schedule are then never a write-only
                // signal. `Full`/`Disconnected` both carry the batch.
                let batch = match e {
                    mpsc::TrySendError::Full(b) => b,
                    mpsc::TrySendError::Disconnected(b) => b,
                };
                self.authority.record_lost_corrector(&batch);
                self.authority.disarm();
                self.authority.schedule_reconciliation();
                Err(BatchRejected)
            }
        }
    }
}

// =============================================================================================
// The sole publisher
// =============================================================================================

/// Typed publication failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// Overlay staging exceeded `MAX_DIRTY_POOLS` — the WHOLE overlay is refused (typed
    /// taint + reconciliation), never a prefix commit.
    CapacityExceeded,
    /// The same pool appears twice as a FINAL write in one overlay.
    DuplicatePool(PoolId),
    /// The write payload's family does not match the pool's kind.
    FamilyScope(PoolId),
    /// Epoch collision (sole-writer invariant violated).
    EpochCollision,
    /// Checked-arithmetic hard disarm reached.
    HardDisarm,
    /// Injected test fault (fail-after-apply path).
    InjectedFault,
}

/// Why pools are poisoned (typed; carried into the journal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoisonReason {
    /// Overlay capacity exceeded — the touch set is not fully known.
    CapacityExceeded,
    /// Replay/stream integrity fault.
    ReplayIntegrity,
    /// Reconciliation found conflicting state.
    ReconcileConflict,
}

/// Test/loom probe phases inside `publish_transaction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishPhase {
    /// The odd writer window is open; nothing applied yet.
    OddOpened,
    /// Mutation `i` has been applied to its family store.
    Applied(usize),
    /// All mutations applied; the closing even store has not happened yet.
    BeforeCloseEven,
}

/// Journal entry (T8 expands to the bounded generational ring + inverse-byte proofs).
#[derive(Debug)]
struct JournalEntry {
    committed_epoch: u64,
    touched: ArrayVec<PoolId, MAX_DIRTY_POOLS>,
}

/// §3.3 sole state publisher. Owns the global epoch, the route-cell table (observe-only),
/// the taint/dark state, and the off-thread command queue receiver. The state thread owns
/// the [`StateWriterPermit`] and passes a [`StateWriteTxn`] witness per publication.
pub struct StatePublisher {
    universe: Arc<Universe>,
    epoch: Arc<StatePublicationEpoch>,
    authority: Arc<PublicationAuthority>,
    route_cells: RouteAuthorityCellTable,
    scratch: DirtyRouteScratch,
    taint_record: TaintAuthorityRecord,
    replay_integrity: Arc<ReplayIntegrityShared>,
    taint_counters: Arc<TaintProducerCounters>,
    dark: bool,
    frozen: bool,
    dark_table: DarkFrontierTable,
    opaque_identity_source: u64,
    cmd_tx: mpsc::SyncSender<StateMutationBatch>,
    cmd_rx: mpsc::Receiver<StateMutationBatch>,
    journal: Vec<JournalEntry>,
    journal_cap: usize,
}

impl StatePublisher {
    /// Construct the sole publisher over the sealed universe.
    pub fn new(universe: Arc<Universe>, route_count: usize, command_queue_depth: usize) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(command_queue_depth);
        Self {
            universe,
            epoch: Arc::new(StatePublicationEpoch::new()),
            authority: Arc::new(PublicationAuthority::new()),
            route_cells: RouteAuthorityCellTable::new(route_count),
            scratch: DirtyRouteScratch::new(route_count),
            taint_record: TaintAuthorityRecord::new(),
            replay_integrity: Arc::new(ReplayIntegrityShared::new()),
            taint_counters: Arc::new(TaintProducerCounters::new()),
            dark: false,
            frozen: false,
            dark_table: DarkFrontierTable::new(),
            opaque_identity_source: 0,
            cmd_tx,
            cmd_rx,
            journal: Vec::new(),
            journal_cap: 1024,
        }
    }

    /// The global publication epoch (shared with readers on other threads).
    pub fn epoch(&self) -> &Arc<StatePublicationEpoch> {
        &self.epoch
    }

    /// Shared authority flags (readers).
    pub fn authority(&self) -> &Arc<PublicationAuthority> {
        &self.authority
    }

    /// The shared D2 fault block.
    pub fn replay_integrity(&self) -> &Arc<ReplayIntegrityShared> {
        &self.replay_integrity
    }

    /// The shared D1b producer counters.
    pub fn taint_counters(&self) -> &Arc<TaintProducerCounters> {
        &self.taint_counters
    }

    /// The §3.2 route-authority cell table (observe-only until WI-08).
    pub fn route_cells(&self) -> &RouteAuthorityCellTable {
        &self.route_cells
    }

    /// The D1 taint authority record.
    pub fn taint_record(&self) -> &TaintAuthorityRecord {
        &self.taint_record
    }

    /// Mint a SEND-ONLY off-thread writer handle.
    pub fn command_sender(&self) -> StateCommandSender {
        StateCommandSender {
            tx: self.cmd_tx.clone(),
            authority: Arc::clone(&self.authority),
        }
    }

    /// §3.3 writer protocol steps 3–8 for one complete transaction overlay.
    pub fn publish_transaction(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        overlay: TransactionOverlay,
    ) -> Result<CommittedEpoch, PublishError> {
        self.publish_transaction_inner(txn, overlay, false, |_| {})
    }

    /// Probe variant (test/loom): `probe` fires at the named [`PublishPhase`]s.
    #[cfg(any(test, loom))]
    pub fn publish_transaction_with_probe(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        overlay: TransactionOverlay,
        probe: impl FnMut(PublishPhase),
    ) -> Result<CommittedEpoch, PublishError> {
        self.publish_transaction_inner(txn, overlay, false, probe)
    }

    /// Fault-injection variant (test): fail after the last apply, before the even close.
    #[cfg(any(test, loom))]
    pub fn publish_transaction_with_fault(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        overlay: TransactionOverlay,
    ) -> Result<CommittedEpoch, PublishError> {
        self.publish_transaction_inner(txn, overlay, true, |_| {})
    }

    /// §3.3 writer protocol:
    /// 3. validate capacity / duplicate final writes / family scope / inverse staging
    ///    BEFORE publication (a failed validation refuses the WHOLE overlay — no window);
    /// 4. compare-exchange the even epoch to ODD with the sole-writer permit;
    /// 5. publish every affected pool snapshot + generation metadata inside the odd window;
    /// 6. commit the in-memory journal (and, while dark, the per-origin frontier row);
    /// 7. release-store the next even epoch;
    /// 8. only after that store may the caller emit dirty work.
    /// A failure AFTER the window opened leaves the epoch ODD (readers decline forever
    /// until restart/reconcile) and DISARMS authority — never an even close over a
    /// partially applied state.
    fn publish_transaction_inner(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        mut overlay: TransactionOverlay,
        inject_fault: bool,
        mut probe: impl FnMut(PublishPhase),
    ) -> Result<CommittedEpoch, PublishError> {
        // ---- Step 3: validate BEFORE the window (refuse-whole, never prefix). ----
        if overlay.overflowed() {
            return Err(PublishError::CapacityExceeded);
        }
        for (i, m) in overlay.mutations.iter().enumerate() {
            if overlay.mutations[..i].iter().any(|prior| prior.pool == m.pool) {
                return Err(PublishError::DuplicatePool(m.pool));
            }
            self.validate_family_scope(m)?;
        }
        // Stage inverse + touched (reads only; still outside the window).
        overlay.inverse.clear();
        overlay.touched.clear();
        for m in overlay.mutations.iter() {
            let prior = self.capture_inverse(m);
            let _ = overlay.inverse.try_push(prior);
            let _ = overlay.touched.try_push(m.pool);
        }
        // ---- Step 4: open the odd writer window. ----
        let odd = match self.epoch.open_odd() {
            Ok(odd) => odd,
            Err(EpochError::Collision) => return Err(PublishError::EpochCollision),
            Err(EpochError::HardDisarm) => {
                self.authority.disarm();
                return Err(PublishError::HardDisarm);
            }
        };
        probe(PublishPhase::OddOpened);
        // ---- Step 5: apply inside the window. ----
        for (i, m) in overlay.mutations.iter().enumerate() {
            if let Err(e) = self.apply_family_write(txn, overlay.origin, m) {
                // Post-validation apply failure: leave ODD, disarm — readers must never
                // accept the partial state.
                self.authority.disarm();
                return Err(e);
            }
            self.universe.pools.bump_generation(m.pool, txn);
            probe(PublishPhase::Applied(i));
        }
        probe(PublishPhase::BeforeCloseEven);
        if inject_fault {
            self.authority.disarm();
            return Err(PublishError::InjectedFault);
        }
        // ---- Step 6: journal + dark-frontier row while still odd. ----
        self.journal_commit(odd.wrapping_add(1), &overlay.touched);
        if self.dark {
            let record = self.frontier_record_for(&overlay.origin, &overlay.point);
            self.dark_table.record_commit(overlay.origin, record);
        }
        // ---- Step 7: close to the next even epoch. ----
        match self.epoch.close_even(odd) {
            Ok(committed) => Ok(committed),
            Err(_) => {
                self.authority.disarm();
                Err(PublishError::HardDisarm)
            }
        }
    }

    /// Family-scope pre-validation (step 3): the write payload's family must match the
    /// pool's kind.
    fn validate_family_scope(&self, m: &PoolStateMutation) -> Result<(), PublishError> {
        let kind = self.universe.pools.meta(m.pool).kind;
        if family_scope_ok(&m.write, kind) {
            Ok(())
        } else {
            Err(PublishError::FamilyScope(m.pool))
        }
    }

    /// Snapshot the pool's CURRENT resident family state as a [`FamilyWrite`] of the SAME variant as
    /// `like`. Reused by [`Self::capture_inverse`] (journal rollback value) and the WI-06 T3 round-5
    /// shadow value-parity gate (the resident side of the CAS compare).
    fn resident_family_write(&self, pool: PoolId, like: &FamilyWrite) -> FamilyWrite {
        let slot = self.universe.pools.meta(pool).state_slot;
        match like {
            FamilyWrite::V2(_) => FamilyWrite::V2(self.universe.v2_state.snapshot(slot)),
            FamilyWrite::V3(_) => FamilyWrite::V3(self.universe.v3_state.snapshot(slot)),
            FamilyWrite::Curve(_) => {
                FamilyWrite::Curve(self.universe.curve_state.snapshot(slot).kernel)
            }
            FamilyWrite::CurveCrypto(_) => FamilyWrite::CurveCrypto(Box::new(
                self.universe.curve_crypto_state.snapshot(slot).as_ref().clone(),
            )),
            FamilyWrite::Dodo(_) => FamilyWrite::Dodo(Box::new(
                self.universe.dodo_state.snapshot(slot).as_ref().clone(),
            )),
            FamilyWrite::BalancerStable(_) => FamilyWrite::BalancerStable(Box::new(
                self.universe.balancer_state.snapshot(slot).kernel.clone(),
            )),
            FamilyWrite::Lb(_) => FamilyWrite::Lb(Box::new(
                self.universe.lb_state.snapshot(slot).kernel.clone(),
            )),
            FamilyWrite::BalancerWeighted(_) => FamilyWrite::BalancerWeighted(Box::new(
                self.universe.balancer_weighted_state.snapshot(slot).as_ref().clone(),
            )),
            FamilyWrite::Fluid(_) => FamilyWrite::Fluid(Box::new(
                self.universe.fluid_state.snapshot(slot).as_ref().clone(),
            )),
            FamilyWrite::Maverick(_) => FamilyWrite::Maverick(Box::new(
                self.universe.maverick_state.snapshot(slot).as_ref().clone(),
            )),
        }
    }

    /// Capture the inverse (current store value) of a staged mutation for the journal.
    fn capture_inverse(&self, m: &PoolStateMutation) -> PoolStateMutation {
        PoolStateMutation {
            pool: m.pool,
            point: m.point.clone(),
            expected_prior_generation: self.universe.pools.generation(m.pool),
            // Inverse (journal rollback) writes are publisher-internal, never a shadow reconstruction.
            pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
            shadow_pre_image: None,
            write: self.resident_family_write(m.pool, &m.write),
        }
    }

    /// Apply one off-thread mutation batch (the publisher's command-drain body): mutations
    /// are ordered by their CANONICAL points (never arrival order — a batch whose points
    /// completed in reverse still commits canonically), and since every family write is a
    /// complete slot value, the NEWEST point per pool is the one FINAL write published.
    pub fn apply_mutation_batch(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        batch: StateMutationBatch,
    ) -> Result<CommittedEpoch, PublishError> {
        // WI-06 T3 fix round 5 (codex round-5 item 5): the batch origin — a `Shadow`-origin full-slot
        // write MUST pass the value-parity (CAS) gate below (carry a fork pre-image that still matches
        // resident); every other origin is exempt (they stamp `expected_prior_generation` at read time).
        let origin = batch.origin;
        let mut mutations: Vec<PoolStateMutation> = batch.mutations.into_iter().collect();
        // Canonical transaction order (stable: incomparable/transitional points keep
        // arrival order among themselves — precedence ENFORCEMENT is WI-07's).
        mutations.sort_by_key(|m| canonical_sort_key(&m.point));
        // One FINAL write per pool: the newest point's complete slot value.
        let mut overlay = TransactionOverlay::new(
            batch.origin,
            mutations
                .last()
                .map(|m| m.point.clone())
                .unwrap_or(MutationPoint::Transitional(TransitionalFeedPoint {
                    boot_id: 0,
                    replay: FeedReplayCursor::new(0, 0),
                    tx_hash: [0u8; 32],
                })),
        );
        let mut finals: Vec<PoolStateMutation> = Vec::with_capacity(mutations.len());
        for m in mutations {
            if let Some(existing) = finals.iter_mut().find(|f| f.pool == m.pool) {
                *existing = m; // later in canonical order ⇒ the final write
            } else {
                finals.push(m);
            }
        }
        // F3 (codex item 5): validate `expected_prior_generation` for each FINAL full-slot write.
        // A full-slot family write computed from generation g is STALE-over-new if the pool's
        // generation has advanced since (another writer committed in between) — applying it would
        // silently lose that intervening delta (the WSS back-to-back-delta bug). Fail-CLOSED: do
        // NOT apply the stale write; poison the pool + schedule reconciliation (resync re-snapshots
        // it). The `UNVALIDATED_PRIOR_GENERATION` sentinel skips the check.
        let mut stale: ArrayVec<PoolId, MAX_DIRTY_POOLS> = ArrayVec::new();
        // WI-06 T3 fix round 4 (codex round-4 item 5): SHADOW pre-replay-seq refusals — kept SEPARATE
        // from `stale` because the disposition differs (resident holds a FRESHER commit, not a lost
        // delta ⇒ resync-only, no poison — see the disposition loop below).
        let mut seq_refused: ArrayVec<PoolId, MAX_DIRTY_POOLS> = ArrayVec::new();
        // WI-06 T3 fix round 5 (codex round-5 item 5): SHADOW value-parity (CAS) refusals — same
        // disposition as `seq_refused` (resident is fresher/diverged, not a lost delta ⇒ resync-only,
        // no poison), tracked separately for clarity.
        let mut parity_refused: ArrayVec<PoolId, MAX_DIRTY_POOLS> = ArrayVec::new();
        finals.retain(|m| {
            // WI-06 T3 fix round 5 (codex round-5 item 5) — SUPERSEDED by round 9 (see the round-9
            // EXPERIMENTAL-PATH HARDENING note at the tick gate below and
            // `codex/PHASE6_DEFERRAL_LEDGER_2026-07-17.md`): the SHADOW value-parity (CAS) gate. Round 5
            // framed this as THE CORRECTNESS claim; round 9 RETRACTED that — the promotion is EXPERIMENTAL,
            // DEFAULT-OFF (`SHADOW_PROMOTION`), and this gate NARROWS but does not close the producer's
            // blessable set. Checked FIRST. codex demonstrated two interleavings the `pre_replay_seq`
            // guard below cannot close: (a) a commit captured mid-odd-window and (b) a REVM fork that
            // simply LAGS a completed commit — in both the shadow's captured seq already equals the
            // committed seq, so the strict-`>` seq compare passes even though the fork read PRE-commit
            // state. This gate closes that seq-guard timing gap FOR THE COMPARED SCALARS (not a producer
            // soundness proof — round 9): a `Shadow`-origin write carries the pool's FORK
            // PRE-STATE image (the resident state the prediction was computed FROM) and applies ONLY IF
            // that pre-image still byte-matches the CURRENT resident (swap-load-bearing fields). If the
            // resident advanced at ANY point the pre-image cannot match ⇒ REFUSE. Fail-closed & TOTAL: a
            // `Shadow`-origin write with NO pre-image is also refused (a shadow promotion must carry one;
            // if its derivation failed the shadow falls to its parity+resync path, never an unguarded
            // submit). Disposition is resync-only (no poison): the resident is fresher/diverged, and the
            // shadow promotion is a non-authoritative RPC-saving optimization whose designed fallback is
            // exactly a targeted resync.
            if origin == BarrierOriginClass::Shadow {
                // Fail-closed totality: a Shadow write MUST carry a pre-image (None ⇒ refuse).
                let Some(pre) = m.shadow_pre_image.as_ref() else {
                    let _ = parity_refused.try_push(m.pool);
                    return false;
                };
                // WI-06 T3 fix round 8 (M3, codex round-8 defect 2): fail-closed SHAPE check. The V3
                // tick/boundary gate below runs only under `(FamilyWrite::V3, FamilyWrite::V3)`;
                // `family_scope_ok` validates only the WRITE against the pool kind; and resident selection
                // keys off the PRE-IMAGE variant. A malformed Shadow batch pairing a mismatched pre-image
                // with the write (e.g. a V2 pre-image + a V3 write) would therefore skip the V3 gate AND
                // mis-target the CAS snapshot. Refuse ANY pre-image/write variant mismatch here — before
                // the family gates and before the resident snapshot is taken. No production builder emits
                // such a batch (`shadow_revm.rs` always pairs matching variants), so this is API
                // hardening — but fail-closed, not fail-open.
                if std::mem::discriminant(pre) != std::mem::discriminant(&m.write) {
                    let _ = parity_refused.try_push(m.pool);
                    return false;
                }
                let resident = self.resident_family_write(m.pool, pre);
                if !shadow_pre_image_matches(pre, &resident) {
                    let _ = parity_refused.try_push(m.pool);
                    return false;
                }
                // WI-06 T3 fix round 7 (L1) + round 8 (M1) + round 9 (N3) — EXPERIMENTAL-PATH HARDENING,
                // NOT a soundness proof. The shadow promotion is EXPERIMENTAL and default-OFF
                // (`SHADOW_PROMOTION`, `shadow_revm.rs`): codex round-9 proved the shadow PRODUCER cannot
                // be made fail-closed at this scope (a whole-transaction repeated-pool round-trip restores
                // final tick+L while retaining a stale interior sqrt; Algebra timepoint/adaptive-fee state
                // is outside `V3State`), so the resident-store write is deferred to WI-09. This gate is
                // the publisher-side DEFENSE for that experimental path when it is explicitly enabled — it
                // narrows, but does not close, the producer's blessable set.
                //
                // The V3-class promotion predicate it enforces is tick-UNCHANGED AND a STRICTLY-INTERIOR
                // landing AND the scalar CAS (above) AND the seq guard (below). Initialized ticks exist
                // only at integer tick-index boundaries, so a tick MOVE (`pre.tick != post.tick`) is
                // refused for EVERY V3-class family (Uni/Sushi/Pancake/Algebra/Kyber) — the fork's tick
                // view need not be trusted and warm-window coverage is irrelevant.
                //
                // Tick-unchanged is NOT sufficient on its own: at an EXACT tick boundary the (tick, L)
                // representation is ambiguous between the pre-cross and post-cross forms, and a
                // fork-lagged endpoint-initializing mint is invisible to every compared scalar. Concretely
                // a resident-only below-range mint `[T-spacing, T]` with the current tick == its upper
                // endpoint initializes `T` with nonzero liquidity-net while changing NO compared scalar; a
                // zero-for-one swap clamped exactly at `sqrt(T)` then leaves the lagging fork storing
                // `T -> T` (a tick check alone would pass) while resident truth crosses `T` and stores
                // `T-1` with the net applied. The divergence lives AT the closed endpoint. N3 (codex
                // round-9 defect 1): we therefore require TRUE range membership
                // `sqrt(post_tick) < post_sqrt < sqrt(post_tick + 1)` via engine-amm's canonical TickMath
                // — the strict `<` on BOTH sides refuses the two adjacent boundary ambiguity points AND
                // any out-of-span sqrt (an inconsistent (tick, sqrt) pair a valid slot0 never has). Only a
                // STRICTLY-INTERIOR tick-unchanged landing (the overwhelming common case) promotes: its
                // price stays in the OPEN interior of one unit-tick span, which holds no initialized-tick
                // boundary in ANY view, so no single-swap cross occurs in either view. The refused set is
                // measure-zero for a legitimate producer (endpoints are single U256 points; out-of-span
                // states are never produced), so it costs the common case nothing. (The builder's slot-4
                // decline — `derive_v3_shadow_states`, K2a — remains a cheap belt on the fork side; it
                // never has to be sound for this gate.)
                if let (FamilyWrite::V3(pre_v3), FamilyWrite::V3(post_v3)) = (pre, &m.write) {
                    let tick_unchanged = match self.universe.pools.meta(m.pool).kind {
                        PoolKind::UniswapV3
                        | PoolKind::SushiV3
                        | PoolKind::PancakeV3
                        | PoolKind::AlgebraV19
                        | PoolKind::KyberSwapElastic => pre_v3.tick == post_v3.tick,
                        // Fail-closed (Fable nit 3): no other kind is a shadow-promoted V3 producer
                        // (`build_shadow_pre_image` emits exactly the five above), so this arm is
                        // unreachable — but if one ever appears, REFUSE (resync) rather than bless.
                        _ => false,
                    };
                    // Interior-landing check (M1 + round-9 N3). Short-circuits behind `tick_unchanged`,
                    // so a tick MOVE still costs ONE i32 compare; the (cached) TickMath runs only on the
                    // tick-unchanged common path. N3 (codex round-9 defect 1): enforce TRUE range
                    // membership `sqrt(post_tick) < post_sqrt < sqrt(post_tick + 1)` — the OPEN interior
                    // of the stored tick's unit span — NOT merely `!= endpoints`. The strict `<` on BOTH
                    // sides excludes the two adjacent boundaries (the M1 ambiguity points) AND every
                    // out-of-span sqrt: a post sqrt that is neither endpoint but lies outside the span is
                    // an inconsistent (tick, sqrt) pair a valid slot0 never has, and is refused rather
                    // than blessed under the wrong tick. `checked_add(1)` fails-closed at `i32::MAX`;
                    // `sqrt_ratio_at_tick` errs at the extreme ticks (post_tick == MAX_TICK ⇒ no
                    // `post_tick + 1`); ANY uncomputable boundary refuses (cannot prove an interior
                    // landing ⇒ refuse).
                    let promotable = tick_unchanged
                        && match post_v3.tick.checked_add(1) {
                            Some(next_tick) => match (
                                engine_amm::scythe::sqrt_ratio_at_tick(post_v3.tick),
                                engine_amm::scythe::sqrt_ratio_at_tick(next_tick),
                            ) {
                                (Ok(at_tick), Ok(at_next)) => {
                                    at_tick < post_v3.sqrt_price_x96
                                        && post_v3.sqrt_price_x96 < at_next
                                }
                                _ => false,
                            },
                            None => false,
                        };
                    if !promotable {
                        let _ = parity_refused.try_push(m.pool);
                        return false;
                    }
                }
            }
            // WI-06 T3 fix round 4 (codex round-4 item 5): the SHADOW pre-replay-seq guard, kept as
            // cheap DEFENSE-IN-DEPTH (one u64) behind the value-parity gate above (which round 5 framed as
            // the correctness claim — SUPERSEDED by round 9: the promotion is experimental/default-OFF, so
            // this is experimental-path defense, not a soundness guarantee). It refuses a stale-fork write
            // whose pool `last_commit_seq` advanced
            // strictly past the shadow's captured `pre_replay_seq` — closing the subset of interleavings
            // where a commit landed DURING the replay AND advanced the seq past the capture. Non-shadow
            // producers set `UNVALIDATED_PRE_REPLAY_SEQ` and are unaffected (they stamp the generation
            // correctly at read time). This does NOT weaken the F3 generation validation.
            if m.pre_replay_seq != UNVALIDATED_PRE_REPLAY_SEQ
                && self.universe.pools.last_commit_seq(m.pool) > m.pre_replay_seq
            {
                let _ = seq_refused.try_push(m.pool);
                return false;
            }
            if m.expected_prior_generation == UNVALIDATED_PRIOR_GENERATION {
                return true;
            }
            if self.universe.pools.generation(m.pool) == m.expected_prior_generation {
                true
            } else {
                let _ = stale.try_push(m.pool);
                false
            }
        });
        // WI-06 round 3 H4 (codex item 3 nit): the pools whose family repair write is PUBLISHED
        // below. `publish_transaction` bumps each of these pools' generation exactly once, so a
        // clear that rides the SAME compound batch for one of them must NOT bump again — one bump
        // per pool per batch. (`try_push` past the cap is harmless: an overflowed overlay is refused
        // by `publish_transaction`, so the clear bumps are moot in that case.)
        let mut published_pools: ArrayVec<PoolId, MAX_DIRTY_POOLS> = ArrayVec::new();
        for m in &finals {
            let _ = published_pools.try_push(m.pool);
        }
        for m in finals {
            overlay.push_mutation(m);
        }
        // Publish the family writes (if any), then apply the param-poison SETs on the SAME
        // drain step so a poisoned pool's generation bump lands with its commit
        // (commit-before-wake — the drain emits the DirtyEvent after this returns).
        let epoch = if overlay.mutations.is_empty() {
            // Poison-only batch: poison + generation bump are plain atomics (not seqlock
            // slots), so no epoch window is opened — the generation bump alone re-triggers
            // readers, which then observe the poison (relaxed load) and refuse. Ordering:
            // `poison_param` (AcqRel) happens-before `bump_generation` (Release), so a reader
            // that Acquire-loads the bumped generation observes the poison.
            CommittedEpoch(self.epoch.epoch.load(Ordering::Acquire))
        } else {
            self.publish_transaction(txn, overlay)?
        };
        for pp in &batch.poison {
            self.universe.pools.poison_param(pp.pool, pp.level, txn);
            self.universe.pools.bump_generation(pp.pool, txn);
        }
        // F4 (codex item 8): execute the SOFT-poison CLEARs AFTER this batch's family writes
        // committed (publisher-owned, under the txn). A REJECTED batch never reaches here, so its
        // clear never runs and the pool stays poisoned (fail-closed) — the off-thread path can no
        // longer un-poison unrepaired state. The generation bump makes the un-poison observed by a
        // reader (commit-before-wake — the drain emits the DirtyEvent after this returns).
        //
        // WI-06 G4 (codex item 8-residual): the clear is DECIDED here, AFTER the stale-refusal set
        // is known, and is doubly gated:
        //   (b) if this pool's repair write was STALE-REFUSED above (in `stale`), skip the clear —
        //       the repair did not apply, so its poison must stay (never a transient NONE on
        //       unrepaired state, and never cleared ahead of the repair that justifies it);
        //   (a) `clear_soft_poison` itself no-ops if a NEWER poison bumped the version past the one
        //       the clear verified against — so a newer queued SOFT poison is never un-poisoned by
        //       an older verified-NoChange clear.
        for (pool, ver) in &batch.clear_soft {
            if stale.iter().any(|s| s == pool)
                || seq_refused.iter().any(|s| s == pool)
                || parity_refused.iter().any(|s| s == pool)
            {
                // Repair refused (stale-over-new OR shadow pre-replay-seq OR shadow value-parity) ⇒ its
                // write did not apply, so leave the pool poisoned / do not clear ahead of an un-applied
                // repair (fail-closed). (Shadow batches carry no `clear_soft`, so this arm is defensive
                // totality.)
                continue;
            }
            self.universe.pools.clear_soft_poison(*pool, *ver, txn);
            // WI-06 round 3 H4 (codex item 3 nit): ONE bump per pool per compound batch. If this
            // pool's family repair write was PUBLISHED above, `publish_transaction` already bumped
            // its generation (and the drain's `touched` set — built from mutations ∪ poison ∪
            // clear_soft — already wakes it), so re-bumping here is a spurious extra advance that
            // would falsely refuse the next corrector. Only bump a STANDALONE clear (no published
            // family write for this pool in this batch); the wake still fires either way.
            if !published_pools.contains(pool) {
                self.universe.pools.bump_generation(*pool, txn);
            }
        }
        // F3: fail-closed disposition of the stale-over-new writes refused above. SOFT-poison each
        // (detection refuses the pool's quotes until it is repaired) + bump its generation (so the
        // refusal is observed — the drain emits its DirtyEvent after commit) + schedule a
        // reconciliation obligation so a resync re-snapshots it. The intermediate delta is NEVER
        // silently lost: the pool goes POISONED-pending-resync rather than accept stale state.
        for pool in &stale {
            self.universe
                .pools
                .poison_param(*pool, crate::pool::PARAM_POISON_SOFT, txn);
            self.universe.pools.bump_generation(*pool, txn);
            // G3(b) (codex item 4-residual): RECORD the poisoned pool for targeted resync (not just
            // a bare counter), so `service_reconciliation_obligations` returns it and a resync
            // re-snapshots it. The `record` implies the obligation; keep `schedule` for the count.
            self.authority.record_stale_resync(*pool);
            self.authority.schedule_reconciliation();
        }
        // WI-06 T3 fix round 4 (codex round-4 item 5): fail-closed disposition of the SHADOW writes
        // refused by the pre-replay-seq guard. Unlike F3's stale-over-new (a LOST delta on an
        // AUTHORITATIVE writer ⇒ poison), a seq-refused SHADOW write means the RESIDENT pool already
        // holds a FRESHER committed value than the stale-fork reconstruction, so we do NOT poison (the
        // resident state is correct, not stale) and do NOT apply the shadow value; we DISCARD it and
        // record the pool for a targeted resync so a canonical re-fetch re-verifies it. This is the
        // publisher-side realization of the shadow's OWN parity/resync fallback — which its
        // submit-time Err path structurally cannot reach, because the batch already enqueued Ok and the
        // staleness is only detectable here at apply. Over-emission of a resync is safe (it only
        // triggers a canonical re-fetch); the shadow promotion is a non-authoritative RPC-saving
        // optimization whose designed fallback is exactly this resync.
        for pool in &seq_refused {
            self.authority.record_stale_resync(*pool);
            self.authority.schedule_reconciliation();
        }
        // WI-06 T3 fix round 5 (codex round-5 item 5): fail-closed disposition of the SHADOW writes
        // refused by the value-parity (CAS) gate — identical to the seq-refusal disposition. The
        // resident holds a value that no longer matches the fork pre-image the prediction was computed
        // FROM (it advanced at some point, or the fork lagged), so we do NOT poison (the resident is
        // fresher/diverged, not a lost delta) and do NOT apply the shadow value; we DISCARD it and
        // record the pool for a targeted resync so a canonical re-fetch re-verifies it. This is the
        // publisher-side realization of the shadow's OWN parity/resync fallback.
        for pool in &parity_refused {
            self.authority.record_stale_resync(*pool);
            self.authority.schedule_reconciliation();
        }
        Ok(epoch)
    }

    /// Drain queued off-thread batches through normal publication (NEVER called inside the
    /// barrier freeze). Returns how many batches committed.
    pub fn drain_commands(&mut self, txn: &mut StateWriteTxn<'_>) -> usize {
        self.drain_commands_each(txn, |_| {})
    }

    /// Drain queued off-thread batches, invoking `on_committed` with the batch's touched pool
    /// set AFTER each batch has been published (committed). This is the §3.3 / guardrail-#4
    /// happens-before seam: an off-thread writer no longer emits its own `DirtyEvent` — instead
    /// the sole state thread emits it HERE, strictly after the covering publication, so a
    /// detection reader can never wake on pre-commit state. Returns how many batches committed.
    pub fn drain_commands_each(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        mut on_committed: impl FnMut(&[PoolId]),
    ) -> usize {
        let mut n = 0;
        while let Ok(batch) = self.cmd_rx.try_recv() {
            // Touched = family-write pools AND param-poison pools (a poisoned pool must also
            // wake detection so the poison takes effect), deduped, bounded by the overlay cap.
            let mut touched: ArrayVec<PoolId, MAX_DIRTY_POOLS> = ArrayVec::new();
            for pid in batch
                .mutations
                .iter()
                .map(|m| m.pool)
                .chain(batch.poison.iter().map(|p| p.pool))
                // F4: a SOFT-clear pool must also wake so detection re-ranks the now-unpoisoned pool.
                .chain(batch.clear_soft.iter().map(|(pool, _ver)| *pool))
            {
                if !touched.contains(&pid) {
                    let _ = touched.try_push(pid);
                }
            }
            if self.apply_mutation_batch(txn, batch).is_ok() {
                on_committed(&touched);
                n += 1;
            }
        }
        n
    }

    /// F4 (codex item 4): CONSUME the disarm's obligations. The permit-holding state thread calls
    /// this each loop iteration; when a rejected off-thread corrector scheduled a reconciliation it
    /// re-applies every recorded lost POISON (poison_param + bump under the txn) — so a rejected
    /// poison never leaves a pool live-unpoisoned — and returns the pools whose lost WRITE must be
    /// resynced (the caller enqueues them into the targeted-resync queue). The pending counter is
    /// consumed so it is not write-only. Returns an empty set when nothing is pending.
    ///
    /// (T3 scope: this closes the "live-unpoisoned / silently-dropped corrector" hole. Re-ARMING
    /// authority still flows through the full [`Self::commit_taint_reconciliation`] barrier — the
    /// deferred T6/T7 dominance-verified re-arm — so this does not clear the disarm latch itself.)
    #[must_use]
    pub fn service_reconciliation_obligations(&mut self, txn: &mut StateWriteTxn<'_>) -> Vec<PoolId> {
        // G3(a): drain poison intents + resync pools + the bare count in ONE atomic take. The
        // RECORDED obligations (not a separately-swapped counter) are the source of truth, so a
        // corrector recorded while a raced counter reset lands is still serviced here.
        let (poisons, resync, pending) = self.authority.take_obligations();
        if poisons.is_empty() && resync.is_empty() && pending == 0 {
            return Vec::new();
        }
        for pp in &poisons {
            self.universe.pools.poison_param(pp.pool, pp.level, txn);
            self.universe.pools.bump_generation(pp.pool, txn);
        }
        resync
    }

    /// Poison `pools` (hard param-poison + generation bump) as one publication.
    pub fn poison_pools(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        pools: &[PoolId],
        reason: PoisonReason,
        point: StatePoint,
    ) -> Result<CommittedEpoch, PublishError> {
        let _ = (reason, point);
        for p in pools {
            self.universe.pools.poison_param(*p, PARAM_POISON_HARD, txn);
            self.universe.pools.bump_generation(*p, txn);
        }
        let cur = self.epoch.epoch.load(Ordering::Acquire);
        let next = cur.wrapping_add(2) & !1;
        self.epoch.epoch.store(next, Ordering::Release);
        Ok(CommittedEpoch(next))
    }

    /// Enter the dark (revoked) state: initialize the per-origin dark-frontier table and
    /// disarm authority. (T7 wires the taint classification that calls this.)
    pub fn begin_dark(&mut self) {
        self.dark = true;
        self.dark_table = DarkFrontierTable::new();
        self.authority.disarm();
    }

    /// Whether authority is dark.
    pub fn is_dark(&self) -> bool {
        self.dark
    }

    /// The dark-frontier table (test/diagnostic view).
    pub fn dark_table(&self) -> &DarkFrontierTable {
        &self.dark_table
    }

    fn frontier_record_for(
        &mut self,
        origin: &BarrierOriginClass,
        point: &MutationPoint,
    ) -> FrontierRecord {
        match origin.dominance() {
            DominanceClass::Feed => match point {
                MutationPoint::Transitional(t) => FrontierRecord::Feed(t.replay),
                MutationPoint::State(_) => FrontierRecord::Absent,
            },
            DominanceClass::CanonicalComparable => match point {
                MutationPoint::State(sp) => match &sp.origin {
                    crate::authority::StateOrigin::Canonical(c) => FrontierRecord::Canonical(*c),
                    crate::authority::StateOrigin::ExactRpc { requested, .. } => {
                        FrontierRecord::Canonical(*requested)
                    }
                    _ => {
                        self.opaque_identity_source += 1;
                        FrontierRecord::Opaque {
                            identity_generation: self.opaque_identity_source,
                        }
                    }
                },
                MutationPoint::Transitional(_) => {
                    self.opaque_identity_source += 1;
                    FrontierRecord::Opaque {
                        identity_generation: self.opaque_identity_source,
                    }
                }
            },
            DominanceClass::ReconcileResetOrSuspend => {
                self.opaque_identity_source += 1;
                FrontierRecord::Opaque {
                    identity_generation: self.opaque_identity_source,
                }
            }
        }
    }

    /// The private D1 re-arm barrier: freeze ALL mutation classes → per-row dominance /
    /// reconcile-reset-suspend verify against the FROZEN table → covering commit at a stable
    /// even epoch → CAS (taint record + fresh even epoch + re-arm) → unfreeze (staged work
    /// replays through NORMAL publication after re-arm, never inside the barrier).
    ///
    /// WI-06 T2 STUB (RED): DRAINS the staged off-thread queue between verify and CAS
    /// (bypassing the dominance verify) and IGNORES the incomparable-row treatments.
    pub(crate) fn commit_taint_reconciliation(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        barrier: CompletedTaintBarrier,
    ) -> RearmOutcome {
        self.commit_taint_reconciliation_with_probe(txn, barrier, |_| {})
    }

    /// Probe variant: fires between the dominance verify and the CAS.
    #[cfg(any(test, loom))]
    pub(crate) fn commit_taint_reconciliation_with_probe(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        barrier: CompletedTaintBarrier,
        probe: impl FnMut(BarrierPhase),
    ) -> RearmOutcome {
        self.commit_taint_reconciliation_inner(txn, barrier, probe)
    }

    #[cfg(not(any(test, loom)))]
    pub(crate) fn commit_taint_reconciliation_with_probe(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        barrier: CompletedTaintBarrier,
        probe: impl FnMut(BarrierPhase),
    ) -> RearmOutcome {
        self.commit_taint_reconciliation_inner(txn, barrier, probe)
    }

    fn commit_taint_reconciliation_inner(
        &mut self,
        txn: &mut StateWriteTxn<'_>,
        barrier: CompletedTaintBarrier,
        mut probe: impl FnMut(BarrierPhase),
    ) -> RearmOutcome {
        if !self.dark {
            return RearmOutcome::Refused(RearmRefusal::NotDark);
        }
        // FREEZE: from here to the CAS nothing may commit except the barrier's OWN covering
        // batch. The publisher IS the single state thread, so the sequence is atomic from
        // the writer's perspective; the staged off-thread queue is NEVER touched inside the
        // barrier — arrivals stage in their bounded queue and replay through NORMAL
        // publication strictly AFTER re-arm.
        self.frozen = true;
        let frozen_table = self.dark_table.clone();

        // Per-row dominance / reconcile-reset-suspend verify against the FROZEN table.
        let verify = self.verify_barrier_rows(&frozen_table, &barrier);
        probe(BarrierPhase::BetweenVerifyAndCas);

        if let Some(refusal) = verify {
            // ANY unsatisfied row ⇒ barrier INCOMPLETE ⇒ refuse (extend and retry). The
            // freeze stops all frontier motion, so catch-up converges.
            self.frozen = false;
            return RearmOutcome::Refused(refusal);
        }

        // Covering commit.
        let committed = match self.apply_mutation_batch(txn, barrier.covering_batch) {
            Ok(c) => c,
            Err(_) => {
                self.frozen = false;
                return RearmOutcome::Refused(RearmRefusal::TaintCountAdvanced);
            }
        };
        // CAS: stamp the taint record + re-arm.
        self.taint_record.stamp(
            txn,
            TaintAuthorityView {
                snapshot: barrier.snapshot,
                rearm_epoch: committed.0,
            },
        );
        self.dark = false;
        self.frozen = false;
        self.authority.rearm();
        RearmOutcome::Rearmed(committed)
    }

    /// Verify every row of the FROZEN table against the barrier (D1 rule 3).
    fn verify_barrier_rows(
        &self,
        frozen: &DarkFrontierTable,
        barrier: &CompletedTaintBarrier,
    ) -> Option<RearmRefusal> {
        for class in BarrierOriginClass::ALL {
            match (class.dominance(), frozen.row(class)) {
                (_, FrontierRecord::Absent) => {}
                (DominanceClass::Feed, FrontierRecord::Feed(cursor)) => {
                    let frontier_block = cursor.sequence.saturating_add(barrier.feed_block_offset);
                    if barrier.anchor.block_number < frontier_block {
                        return Some(RearmRefusal::AnchorBehindFeedFrontier);
                    }
                }
                (DominanceClass::CanonicalComparable, FrontierRecord::Canonical(row)) => {
                    // Lineage-aware dominance via canonical state_order (never bare height):
                    // the anchor must be Newer than (or identical to) the row.
                    let rel = crate::authority::state_order(
                        &canonical_probe_point(&row),
                        &canonical_probe_point(&barrier.anchor),
                        None,
                    );
                    if !matches!(
                        rel,
                        StateOrderRelation::Newer | StateOrderRelation::DuplicateIdentical
                    ) {
                        return Some(RearmRefusal::AnchorDoesNotDominateCanonicalRow(class));
                    }
                }
                (DominanceClass::ReconcileResetOrSuspend, _) => {
                    if barrier.treatments[class as usize].is_none() {
                        return Some(RearmRefusal::IncomparableRowUntreated(class));
                    }
                }
                // A row recorded under the WRONG record kind for its class is itself an
                // incomparable surface: require treatment.
                (_, _) => {
                    if barrier.treatments[class as usize].is_none() {
                        return Some(RearmRefusal::IncomparableRowUntreated(class));
                    }
                }
            }
        }
        if !self.taint_counters.current_matches(&barrier.snapshot) {
            return Some(RearmRefusal::TaintCountAdvanced);
        }
        if barrier.fork == SurfaceObligation::Unmet {
            return Some(RearmRefusal::ForkObligationUnmet);
        }
        if barrier.mirror == SurfaceObligation::Unmet {
            return Some(RearmRefusal::MirrorObligationUnmet);
        }
        None
    }

    fn journal_commit(&mut self, committed_epoch: u64, touched: &ArrayVec<PoolId, MAX_DIRTY_POOLS>) {
        if self.journal.len() >= self.journal_cap {
            self.journal.remove(0);
        }
        self.journal.push(JournalEntry {
            committed_epoch,
            touched: touched.clone(),
        });
    }

    /// Apply one family write with the slot-preserving merge `commit_prediction` performs
    /// today (tokens/poolId maps preserved for Curve/BalancerStable/LB; self-carrying slots
    /// written through). Family scope is validated against the pool's kind.
    fn apply_family_write(
        &self,
        txn: &StateWriteTxn<'_>,
        origin: BarrierOriginClass,
        m: &PoolStateMutation,
    ) -> Result<(), PublishError> {
        let meta = self.universe.pools.meta(m.pool);
        let slot = meta.state_slot;
        if !family_scope_ok(&m.write, meta.kind) {
            return Err(PublishError::FamilyScope(m.pool));
        }
        // WI-06 T3 fix round 7 (L3, Fable nit 4): merge (field-preserving) apply semantics are keyed on
        // the batch ORIGIN, not on `shadow_pre_image.is_some()`. ONLY a Shadow-origin write — the class
        // the value-parity CAS gate guards, and the only production producer of a pre-image — takes merge
        // semantics; every other origin full-replaces. (A Shadow write that reached apply always carries
        // a pre-image: the None case is refused at the gate.)
        let shadow = origin == BarrierOriginClass::Shadow;
        match &m.write {
            FamilyWrite::V2(new_state) => {
                if shadow {
                    // WI-06 T3 fix round 6 (K5): a SHADOW V2 promotion writes the reserves ONLY and
                    // PRESERVES the resident `block_timestamp_last`. The shadow reconstruction cannot
                    // observe the post-swap `blockTimestampLast` (the packed-slot match is
                    // timestamp-agnostic and the prediction forces it to 0), so a full replace would
                    // REGRESS a field detection reads, WSS carries, and the resident/prediction hashes
                    // read. Merge semantics keep the resident timeline consistent; the reserves (the
                    // fork-derived, CAS-compared fields) are the only thing the swap moved.
                    let mut merged = self.universe.v2_state.snapshot(slot);
                    merged.reserves.reserve0 = new_state.reserves.reserve0;
                    merged.reserves.reserve1 = new_state.reserves.reserve1;
                    self.universe.v2_state.set(slot, merged, txn);
                } else {
                    self.universe.v2_state.set(slot, new_state.clone(), txn);
                }
            }
            FamilyWrite::V3(new_state) => {
                if shadow {
                    // WI-06 T3 fix round 6 (K2 part b) + round 7 (L2): a SHADOW V3 promotion replaces
                    // ONLY the swap-load-bearing, fork-derived, CAS-compared scalar head
                    // (sqrt/tick/liquidity/fee). It MERGES, PRESERVING every field the fork did NOT
                    // derive and the CAS did NOT compare — the resident `initialised_ticks` (the warm
                    // tick window) AND the resident `tick_spacing`. Both are resident-seeded static
                    // fields; a shadow V3 promotion is only ever emitted for a tick-UNCHANGED, interior
                    // (non-boundary) landing (provably no-cross under the L1/M1 predicate), which moves
                    // neither — so carrying the freshest resident values forward is exact, and the write
                    // never regresses them to a
                    // promotion-time snapshot (e.g. a stale fork spacing). (Non-shadow V3 writes, e.g.
                    // the authoritative resync, full-replace below.)
                    let mut merged = self.universe.v3_state.snapshot(slot);
                    merged.sqrt_price_x96 = new_state.sqrt_price_x96;
                    merged.tick = new_state.tick;
                    merged.liquidity = new_state.liquidity;
                    merged.fee_pips = new_state.fee_pips;
                    self.universe.v3_state.set_recycled(slot, merged, txn);
                } else {
                    self.universe.v3_state.set_recycled(slot, new_state.clone(), txn);
                }
            }
            FamilyWrite::Curve(new_kernel) => {
                let cur = self.universe.curve_state.snapshot(slot);
                self.universe.curve_state.set(
                    slot,
                    crate::curve_state::CurveSlot::new(*new_kernel, cur.tokens),
                    txn,
                );
            }
            FamilyWrite::CurveCrypto(new_slot) => {
                self.universe.curve_crypto_state.set(slot, (**new_slot).clone(), txn);
            }
            FamilyWrite::Dodo(new_state) => {
                if shadow {
                    // WI-06 T3 fix round 8 (M2, codex round-8 defect 1): a SHADOW Dodo promotion MERGES —
                    // it writes the compared PMM/quote fields (the swap-load-bearing state) and PRESERVES
                    // the resident `block_number`/`log_index` provenance stamps (and the pool identity).
                    // The shadow reconstruction decodes pre/post at `block_number = 0`/`log_index = 0` (it
                    // cannot observe the post-swap stamps) and the value-parity CAS excludes them, so a
                    // full replace would REGRESS a resident nonzero stamp to zero. The receipt
                    // reconciliation reads FULL `DodoState` equality (`pipeline.rs`), so that regression
                    // would change `changed`/generation-bump/wake behavior. Merge keeps the resident
                    // timeline consistent — exactly the V2 reserves-only / V3 scalar-head pattern.
                    let mut merged = self.universe.dodo_state.snapshot(slot).as_ref().clone();
                    merged.pool_class = new_state.pool_class;
                    merged.i = new_state.i;
                    merged.k = new_state.k;
                    merged.base_reserve = new_state.base_reserve;
                    merged.quote_reserve = new_state.quote_reserve;
                    merged.base_target = new_state.base_target;
                    merged.quote_target = new_state.quote_target;
                    merged.r_state = new_state.r_state;
                    merged.lp_fee_rate_d18 = new_state.lp_fee_rate_d18;
                    merged.mt_fee_rate_d18 = new_state.mt_fee_rate_d18;
                    // WI-06 T3 fix round 9 (N4, codex round-9 defect 2) + round-10 nit 3 (wording): DERIVE
                    // `fee_ppm` from the two POST fee rates written just above (`new_state`'s lp+mt) — the
                    // invariant every normal Dodo producer maintains (`dodo_v2::apply_swap` /
                    // `dodo_state_from_view`) — NEVER trust the batch's `fee_ppm`. `fee_ppm` is
                    // assigned-but-NOT-compared by the value-parity CAS (which compares the PRE-IMAGE rates
                    // against resident, NOT these written post rates), so a malformed batch could otherwise
                    // land an `fee_ppm` inconsistent with its rates; recomputing from the written rates
                    // makes the batch value irrelevant. A canonical producer keeps its post rates
                    // consistent, so the derivation is total on the normal path. HONESTY (round-10 nit 3):
                    // on a derivation/overflow failure this does NOT refuse the mutation — it falls back to
                    // the RESIDENT `fee_ppm` and still lands the rest of the merge (a rate/`fee_ppm`
                    // inconsistency on the EXPERIMENTAL, default-OFF path). Making Shadow-Dodo REFUSE the
                    // whole mutation on derivation failure is a WI-09 precondition (see the WI-09 residuals
                    // in `codex/PHASE6_DEFERRAL_LEDGER_2026-07-17.md`).
                    merged.fee_ppm = merged
                        .lp_fee_rate_d18
                        .checked_add(merged.mt_fee_rate_d18)
                        .and_then(|total| engine_amm::dodo_v2::fee_rate_to_ppm(total).ok())
                        .unwrap_or(merged.fee_ppm);
                    // `address`/`base_token`/`quote_token` (identity) and `block_number`/`log_index`
                    // (provenance) ride the resident snapshot.
                    self.universe.dodo_state.set(slot, merged, txn);
                } else {
                    self.universe.dodo_state.set(slot, (**new_state).clone(), txn);
                }
            }
            FamilyWrite::BalancerStable(new_kernel) => {
                let cur = self.universe.balancer_state.snapshot(slot);
                self.universe.balancer_state.set(
                    slot,
                    crate::balancer_state::BalancerStableSlot {
                        kernel: (**new_kernel).clone(),
                        tokens: cur.tokens,
                        pool_id: cur.pool_id,
                    },
                    txn,
                );
            }
            FamilyWrite::Lb(new_kernel) => {
                let cur = self.universe.lb_state.snapshot(slot);
                self.universe.lb_state.set(
                    slot,
                    crate::lb_state::LbSlot {
                        kernel: (**new_kernel).clone(),
                        token0: cur.token0,
                        token1: cur.token1,
                    },
                    txn,
                );
            }
            FamilyWrite::BalancerWeighted(new_slot) => {
                self.universe.balancer_weighted_state.set(slot, (**new_slot).clone(), txn);
            }
            FamilyWrite::Fluid(new_snapshot) => {
                self.universe.fluid_state.set(slot, (**new_snapshot).clone(), txn);
            }
            FamilyWrite::Maverick(new_state) => {
                self.universe.maverick_state.set(slot, (**new_state).clone(), txn);
            }
        }
        Ok(())
    }
}

/// Barrier probe phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierPhase {
    /// After the per-row dominance verify, before the CAS.
    BetweenVerifyAndCas,
}

/// Canonical sort key for batch ordering: canonical-comparable points order by
/// `(block, tx_index, log_index)`; transitional/other points sort FIRST (kept in arrival
/// order by the stable sort — WI-07 owns their precedence rules).
fn canonical_sort_key(point: &MutationPoint) -> (u64, u64, u64) {
    match point {
        MutationPoint::State(sp) => match &sp.origin {
            crate::authority::StateOrigin::Canonical(c)
            | crate::authority::StateOrigin::ExactRpc { requested: c, .. } => (
                c.block_number,
                c.tx_index.map_or(0, |t| u64::from(t) + 1),
                c.log_index.map_or(0, |l| u64::from(l) + 1),
            ),
            _ => (0, 0, 0),
        },
        MutationPoint::Transitional(_) => (0, 0, 0),
    }
}

/// WI-06 T3 fix round 5 (codex round-5 item 5): the SHADOW value-parity (CAS) compare. A shadow
/// promotion carries the pool's FORK PRE-STATE image — the resident state the fork read and computed
/// the prediction FROM. The publisher applies the shadow write ONLY IF this pre-image still equals the
/// CURRENT resident state on the COMPARED fields below: if the resident advanced on ANY compared field
/// (mid-commit, pre-capture, or a lagging fork the seq guard cannot see) the pre-image cannot match and
/// the write is REFUSED (resync-only). This is an exact byte-compare on those fields — a shadow write
/// lands ONLY on a resident slot byte-identical (on the compared fields) to what the fork replayed FROM.
/// (The tick-cross class the value compare cannot see is closed SEPARATELY by the uniform tick gate in
/// `apply_mutation_batch` — L1 — which promotes only tick-UNCHANGED swaps.)
///
/// Compares the fields a swap MOVES plus the dynamic fee — deliberately EXCLUDING resident-seeded or
/// bookkeeping fields whose representation legitimately differs between a fork getter read and a resync
/// WITHOUT indicating a state advance, and which the shadow apply MERGE preserves rather than replaces:
/// V2 `block_timestamp_last`; V3 `tick_spacing` and the `initialised_ticks` window (both resident-seeded,
/// preserved by the merge, and unmoved by a tick-unchanged swap); Dodo `block_number`/`log_index`.
/// `fee_pips` IS compared (K3) — Algebra's fee is dynamic and a direct quote input; for the
/// immutable-fee families it matches structurally. Including a preserved field would spuriously REFUSE a
/// genuinely-unchanged pool (e.g. one whose resident state a prior shadow promotion left with a zeroed
/// `block_timestamp_last`). A mismatched or unsupported family pairing returns `false` (fail-closed) —
/// only V2/V3/Dodo are shadow-promoted.
pub fn shadow_pre_image_matches(pre: &FamilyWrite, resident: &FamilyWrite) -> bool {
    match (pre, resident) {
        (FamilyWrite::V2(a), FamilyWrite::V2(b)) => {
            a.reserves.reserve0 == b.reserves.reserve0 && a.reserves.reserve1 == b.reserves.reserve1
        }
        (FamilyWrite::V3(a), FamilyWrite::V3(b)) => {
            // WI-06 T3 fix round 6 (K3): `fee_pips` is in the compare tuple — Algebra's fee is DYNAMIC
            // (a fee-only resident commit exists) and a direct quote input, so a fork lagging it (equal
            // sqrt/tick/liquidity, stale fee) must REFUSE, not overwrite the fresher fee. For
            // UniV3/Sushi/Pancake/Kyber the fee is swap-invariant and taken from the resident base, so
            // `a.fee_pips == b.fee_pips` holds structurally — no false refusal. (`initialised_ticks` /
            // `tick_spacing` are NOT compared: the shadow V3 apply MERGE PRESERVES both resident fields
            // and the uniform rule promotes only tick-UNCHANGED swaps — L1/L2 — so a promotion never
            // replaces them.)
            a.sqrt_price_x96 == b.sqrt_price_x96
                && a.tick == b.tick
                && a.liquidity == b.liquidity
                && a.fee_pips == b.fee_pips
        }
        (FamilyWrite::Dodo(a), FamilyWrite::Dodo(b)) => {
            a.pool_class == b.pool_class
                && a.base_reserve == b.base_reserve
                && a.quote_reserve == b.quote_reserve
                && a.i == b.i
                && a.k == b.k
                && a.base_target == b.base_target
                && a.quote_target == b.quote_target
                && a.r_state == b.r_state
                && a.lp_fee_rate_d18 == b.lp_fee_rate_d18
                && a.mt_fee_rate_d18 == b.mt_fee_rate_d18
        }
        // No other family is shadow-promoted (build_shadow_prediction returns None); a mismatched or
        // unsupported pairing never matches ⇒ the write is refused (fail-closed).
        _ => false,
    }
}

/// Family-scope compatibility of one write payload against the pool's kind.
fn family_scope_ok(write: &FamilyWrite, kind: PoolKind) -> bool {
    matches!(
        (write, kind),
        (
            FamilyWrite::V2(_),
            PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 | PoolKind::SolidlyV3
        ) | (
            FamilyWrite::V3(_),
            PoolKind::UniswapV3
                | PoolKind::SushiV3
                | PoolKind::PancakeV3
                | PoolKind::AlgebraV19
                | PoolKind::KyberSwapElastic
                | PoolKind::UniswapV4
        ) | (FamilyWrite::Curve(_), PoolKind::CurveStableSwapNg)
            | (FamilyWrite::CurveCrypto(_), PoolKind::CurveCryptoSwap)
            | (FamilyWrite::Dodo(_), PoolKind::DodoV2)
            | (FamilyWrite::BalancerStable(_), PoolKind::BalancerStable)
            | (FamilyWrite::BalancerWeighted(_), PoolKind::BalancerWeighted)
            | (FamilyWrite::Lb(_), PoolKind::TraderJoeLb)
            | (FamilyWrite::Fluid(_), PoolKind::FluidDex)
            | (FamilyWrite::Maverick(_), PoolKind::MaverickV2)
    )
}

/// Wrap a bare canonical cursor as a probe `StatePoint` for lineage comparison.
fn canonical_probe_point(c: &CanonicalCursor) -> StatePoint {
    StatePoint {
        schema_version: 1,
        chain_id: 0,
        origin: crate::authority::StateOrigin::Canonical(*c),
        reported_l2_block: None,
        observation: None,
        reconciliation_anchor: None,
        reconciliation_generation: NonZeroU64::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::UniverseBuilder;
    use crate::ids::{TokenId, VenueId};
    use crate::pool::{PoolKind, PARAM_POISON_NONE, PARAM_POISON_SOFT};
    use crate::token::RiskClass;
    use crate::v2_state::{V2Reserves256, V2State};
    use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as StdOrdering};
    use std::sync::mpsc as std_mpsc;
    use std::sync::Arc as StdArc;

    fn v2(reserve0: u128, reserve1: u128) -> V2State {
        V2State {
            reserves: V2Reserves256 {
                reserve0,
                reserve1,
                block_timestamp_last: 0,
            },
        }
    }

    /// Two-token, three-V2-pool universe for substrate tests.
    fn test_universe() -> (StdArc<Universe>, [PoolId; 3]) {
        let mut b = UniverseBuilder::new();
        let t0 = b.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let t1 = b.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let mut pools = Vec::new();
        for i in 0..3u8 {
            let mut addr = [0xA0; 20];
            addr[19] = i;
            pools.push(
                b.add_v2_pool(
                    addr,
                    PoolKind::UniswapV2,
                    t0,
                    t1,
                    VenueId(0),
                    30,
                    v2(1_000_000, 1_000_000),
                )
                .expect("add pool"),
            );
        }
        (
            StdArc::new(b.build()),
            [pools[0], pools[1], pools[2]],
        )
    }

    fn transitional(seq: u64, ord: u32) -> MutationPoint {
        MutationPoint::Transitional(TransitionalFeedPoint {
            boot_id: 0xB007,
            replay: FeedReplayCursor::new(seq, ord),
            tx_hash: [0xAA; 32],
        })
    }

    fn canonical_point(block: u64, hash: u8) -> MutationPoint {
        MutationPoint::State(StatePoint {
            schema_version: 1,
            chain_id: 0,
            origin: crate::authority::StateOrigin::Canonical(CanonicalCursor {
                block_number: block,
                block_hash: [hash; 32],
                state_root: [0x77; 32],
                tx_index: None,
                log_index: None,
            }),
            reported_l2_block: None,
            observation: None,
            reconciliation_anchor: None,
            reconciliation_generation: NonZeroU64::MIN,
        })
    }

    fn v2_mutation(pool: PoolId, point: MutationPoint, r0: u128, r1: u128) -> PoolStateMutation {
        PoolStateMutation {
            pool,
            point,
            // These fixtures test ordering/barrier/epoch behavior, not F3 stale-over-new — skip
            // the generation check (a real off-thread producer sets the observed generation).
            expected_prior_generation: UNVALIDATED_PRIOR_GENERATION,
            pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
            shadow_pre_image: None,
            write: FamilyWrite::V2(v2(r0, r1)),
        }
    }

    fn reserves(u: &Universe, pool: PoolId) -> (u128, u128) {
        let slot = u.pools.meta(pool).state_slot;
        let s = u.v2_state.snapshot(slot);
        (s.reserves.reserve0, s.reserves.reserve1)
    }

    fn tick_info(t: i32, net: i128) -> engine_amm::TickInfo {
        engine_amm::TickInfo {
            tick: t,
            liquidity_net: net,
        }
    }

    fn v3(
        sqrt: u128,
        tick: i32,
        liquidity: u128,
        fee_pips: u32,
        ticks: Vec<engine_amm::TickInfo>,
    ) -> crate::v3_state::V3State {
        crate::v3_state::V3State {
            sqrt_price_x96: alloy_primitives::U256::from(sqrt),
            tick,
            liquidity,
            fee_pips,
            tick_spacing: 60,
            initialised_ticks: ticks,
        }
    }

    /// Two-token universe with one V3-state-backed pool (of the given kind) carrying a 2-tick warm window.
    fn test_universe_v3_kind(kind: PoolKind) -> (StdArc<Universe>, PoolId) {
        let mut b = UniverseBuilder::new();
        let t0 = b.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let t1 = b.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let pool = b
            .add_v3_pool(
                [0xB3; 20],
                kind,
                t0,
                t1,
                VenueId(0),
                3_000,
                60,
                v3(1_000_000, 100, 5_000_000, 3_000, vec![tick_info(-60, 111), tick_info(120, -111)]),
            )
            .expect("add v3 pool");
        (StdArc::new(b.build()), pool)
    }

    fn test_universe_v3() -> (StdArc<Universe>, PoolId) {
        test_universe_v3_kind(PoolKind::UniswapV3)
    }

    fn v3_resident(u: &Universe, pool: PoolId) -> crate::v3_state::V3State {
        let slot = u.pools.meta(pool).state_slot;
        u.v3_state.snapshot(slot)
    }

    /// A DODO V2 state with the given reserves and provenance stamps; all other PMM fields fixed.
    fn dodo(
        base_reserve: u128,
        quote_reserve: u128,
        block_number: u64,
        log_index: u32,
    ) -> engine_amm::dodo_v2::DodoState {
        use alloy_primitives::{Address, U256};
        engine_amm::dodo_v2::DodoState {
            address: Address::from([0xD0; 20]),
            pool_class: engine_amm::dodo_v2::DodoPoolClass::Dvm,
            base_token: Address::from([0x11; 20]),
            quote_token: Address::from([0x22; 20]),
            i: U256::from(2u64),
            k: U256::from(1u64),
            base_reserve: U256::from(base_reserve),
            quote_reserve: U256::from(quote_reserve),
            base_target: U256::from(100u64),
            quote_target: U256::from(200u64),
            r_state: engine_amm::dodo_v2::DodoRState::One,
            lp_fee_rate_d18: U256::from(3_000_000_000_000_000u64),
            mt_fee_rate_d18: U256::from(1_000_000_000_000_000u64),
            fee_ppm: 4_000,
            block_number,
            log_index,
        }
    }

    /// Two-token universe with one DODO V2 pool carrying the given initial snapshot.
    fn test_universe_dodo(initial: engine_amm::dodo_v2::DodoState) -> (StdArc<Universe>, PoolId) {
        let mut b = UniverseBuilder::new();
        let t0 = b.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let t1 = b.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let pool = b
            .add_dodo_pool([0xD0; 20], t0, t1, VenueId(0), 4_000, initial)
            .expect("add dodo pool");
        (StdArc::new(b.build()), pool)
    }

    fn dodo_resident(u: &Universe, pool: PoolId) -> engine_amm::dodo_v2::DodoState {
        let slot = u.pools.meta(pool).state_slot;
        u.dodo_state.snapshot(slot).as_ref().clone()
    }

    // ------------------------------------------------------------------------------------
    // §3.3 writer/reader protocol REDs
    // ------------------------------------------------------------------------------------

    /// §3.3: a multi-pool prediction publishes as ONE transaction epoch — the writer window
    /// is odd for the WHOLE apply span and the commit advances the epoch to the next even
    /// exactly once.
    #[test]
    fn multi_pool_prediction_publishes_one_transaction_epoch_atomically() {
        let (u, [a, b, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(7, 0));
        overlay.push_mutation(v2_mutation(a, transitional(7, 0), 111, 222));
        overlay.push_mutation(v2_mutation(b, transitional(7, 0), 333, 444));

        let before = publisher.epoch().raw();
        assert_eq!(before & 1, 0, "boot epoch is even");
        let mut observed_odd_during_apply = true;
        let epoch = StdArc::clone(publisher.epoch());
        let committed = publisher
            .publish_transaction_with_probe(&mut txn, overlay, |phase| {
                if let PublishPhase::Applied(_) = phase {
                    if epoch.raw() & 1 == 0 {
                        observed_odd_during_apply = false;
                    }
                }
            })
            .expect("publish");
        assert!(
            observed_odd_during_apply,
            "the epoch must be ODD (writer window open) at every point where a family store \
             has been partially mutated — an even epoch mid-apply is exactly the FL-08 torn \
             window"
        );
        assert_eq!(
            committed.0,
            before + 2,
            "one transaction advances the epoch by exactly one even step"
        );
        assert_eq!(reserves(&u, a), (111, 222));
        assert_eq!(reserves(&u, b), (333, 444));
    }

    /// §3.3 reader protocol: a barrier-controlled concurrent reader must NEVER accept a torn
    /// pool set (pool A new / pool B old) under a stable even epoch.
    #[test]
    fn transaction_epoch_reader_never_accepts_torn_pool_set() {
        let (u, [a, b, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Both pools always carry EQUAL reserve pairs per committed transaction; a torn read
        // is a pair where a != b.
        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(1, 0));
        overlay.push_mutation(v2_mutation(a, transitional(1, 0), 1, 1));
        overlay.push_mutation(v2_mutation(b, transitional(1, 0), 1, 1));
        publisher.publish_transaction(&mut txn, overlay).expect("seed");

        let (probe_tx, probe_rx) = std_mpsc::channel::<()>();
        let (done_tx, done_rx) = std_mpsc::channel::<Option<(u128, u128)>>();
        let u_reader = StdArc::clone(&u);
        let epoch = StdArc::clone(publisher.epoch());
        let reader = std::thread::spawn(move || {
            // Wait for the mid-apply signal, then take exactly ONE epoch-guarded read.
            probe_rx.recv().expect("probe signal");
            let read = epoch_read_pair(&epoch, || {
                let ra = reserves(&u_reader, a).0;
                let rb = reserves(&u_reader, b).0;
                (ra, rb)
            });
            done_tx.send(read.map(|(_, v)| v)).expect("send result");
        });

        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(2, 0));
        overlay.push_mutation(v2_mutation(a, transitional(2, 0), 2, 2));
        overlay.push_mutation(v2_mutation(b, transitional(2, 0), 2, 2));
        publisher
            .publish_transaction_with_probe(&mut txn, overlay, |phase| {
                if phase == PublishPhase::Applied(0) {
                    // Pool A committed, pool B not yet: release the reader NOW.
                    probe_tx.send(()).expect("signal reader");
                    // Give the reader its full read while we are mid-apply.
                    let result = done_rx.recv().expect("reader result");
                    if let Some((ra, rb)) = result {
                        assert_eq!(
                            ra, rb,
                            "the epoch-guarded reader ACCEPTED a torn pool set (a={ra}, \
                             b={rb}) under a stable even epoch — the writer must hold the \
                             odd window across the whole apply span (FL-08)"
                        );
                    }
                }
            })
            .expect("publish");
        reader.join().expect("reader joins");
    }

    /// §3.3: a second writer window while one is open is a COLLISION — typed decline, no
    /// spin, no silent success (the sole-writer invariant is load-bearing).
    #[test]
    fn transaction_epoch_collision_declines_without_spinning() {
        let epoch = StatePublicationEpoch::new();
        let odd = epoch.open_odd().expect("first open");
        assert_eq!(odd & 1, 1);
        assert_eq!(
            epoch.open_odd(),
            Err(EpochError::Collision),
            "opening a second writer window while odd must be a typed Collision decline"
        );
        // The open window is still intact and closes normally.
        let committed = epoch.close_even(odd).expect("close");
        assert_eq!(committed.0, odd + 1);
    }

    /// §3.3: a failure AFTER the writer window opened must leave the epoch ODD (readers
    /// decline) and authority DISARMED — never close even over a partially applied state.
    #[test]
    fn publication_failure_leaves_epoch_odd_and_authority_disarmed() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(3, 0));
        overlay.push_mutation(v2_mutation(a, transitional(3, 0), 9, 9));
        let result = publisher.publish_transaction_with_fault(&mut txn, overlay);
        assert!(result.is_err(), "the injected fault must surface");
        assert_eq!(
            publisher.epoch().raw() & 1,
            1,
            "a mid-publication failure must leave the epoch ODD — closing even would let \
             readers accept the partially applied state"
        );
        assert!(
            publisher.authority().is_disarmed(),
            "a mid-publication failure must disarm publication authority"
        );
    }

    // ------------------------------------------------------------------------------------
    // F1 (codex item 1): universe-branded permit ↔ store binding
    // ------------------------------------------------------------------------------------

    /// F1 RED→GREEN: a `StateWriteTxn` minted for ONE universe must be UNUSABLE against a
    /// DIFFERENT universe's family store. A throwaway builder's permit is a forged writer —
    /// witnessing another universe's store with it must fail-CLOSED (panic), never silently
    /// apply out-of-band resident state. Before branding this write succeeded (no panic) =
    /// the forgeable-permit class; after branding the brand mismatch panics.
    #[test]
    #[should_panic(expected = "does not match store universe")]
    fn foreign_universe_permit_is_refused_by_family_store() {
        let (ua, [a, _, _]) = test_universe();
        let slot = ua.pools.meta(a).state_slot;
        // A DIFFERENT builder mints a DIFFERENT universe brand.
        let (_, mut forged) = UniverseBuilder::new().seal().into_parts();
        let txn = forged.begin_txn();
        // Cross-universe witness: brand(forged) != brand(ua stores) ⇒ refuse.
        ua.v2_state.set(slot, v2(1, 2), &txn);
    }

    /// F1 positive control: the sole permit sealed WITH a universe writes that universe's own
    /// stores (matching brand) — the legitimate sole-writer path is unaffected by branding.
    #[test]
    fn sealed_permit_writes_its_own_universe() {
        let mut b = UniverseBuilder::new();
        let t0 = b.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let t1 = b.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let pool = b
            .add_v2_pool([0xA0; 20], PoolKind::UniswapV2, t0, t1, VenueId(0), 30, v2(1, 1))
            .expect("add pool");
        let (universe, mut permit) = b.seal().into_parts();
        let slot = universe.pools.meta(pool).state_slot;
        let txn = permit.begin_txn();
        universe.v2_state.set(slot, v2(5, 6), &txn);
        assert_eq!(reserves(&universe, pool), (5, 6));
    }

    /// F1: a foreign-universe witness is refused at the POOL-TABLE mutators too (bump /
    /// poison share the same fail-closed brand check as the family stores).
    #[test]
    #[should_panic(expected = "does not match store universe")]
    fn foreign_universe_permit_is_refused_by_pool_bump() {
        let (ua, [a, _, _]) = test_universe();
        let (_, mut forged) = UniverseBuilder::new().seal().into_parts();
        let txn = forged.begin_txn();
        ua.pools.bump_generation(a, &txn);
    }

    /// F1: `test_mint()` mints the WILDCARD brand, so substrate/loom/downstream-test writers
    /// can legitimately witness a store they did not seal (the test affordance is unreachable
    /// in production — cfg `test`/`loom`/`test-permit`).
    #[test]
    fn test_mint_wildcard_writes_any_universe() {
        let (ua, [a, _, _]) = test_universe();
        let slot = ua.pools.meta(a).state_slot;
        let mut permit = StateWriterPermit::test_mint();
        let txn = permit.begin_txn();
        ua.v2_state.set(slot, v2(7, 8), &txn);
        assert_eq!(reserves(&ua, a), (7, 8));
    }

    /// §3.3: epoch arithmetic is CHECKED — approaching `u64::MAX - 2` hard-disarms instead
    /// of wrapping to a previously observed even value (ABA).
    #[test]
    fn transaction_epoch_wrap_disarms_instead_of_aba() {
        let epoch = StatePublicationEpoch::with_value(u64::MAX - 5);
        // One full open/close cycle fits below the boundary.
        let odd = epoch.open_odd().expect("open below boundary");
        let committed = epoch.close_even(odd).expect("close below boundary");
        assert_eq!(committed.0, u64::MAX - 3);
        // The NEXT window would reach the boundary: hard disarm, typed error, NO wrap.
        let second = epoch.open_odd();
        assert_eq!(
            second,
            Err(EpochError::HardDisarm),
            "reaching the hard-disarm boundary must refuse with a typed error, not wrap"
        );
        assert!(
            epoch.is_disarmed(),
            "the epoch must be terminally disarmed at the boundary"
        );
        assert!(
            epoch.load_stable().is_none(),
            "no reader may see a stable epoch after hard disarm — wrapping back to a \
             previously observed even value would be ABA"
        );
    }

    /// §3.3: exceeding `MAX_DIRTY_POOLS` is typed taint for the WHOLE overlay — no prefix
    /// commit, no silent drop of the overflow.
    #[test]
    fn overlay_capacity_exceeded_taints_never_prefix_commits() {
        let (u, [a, b, c]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before_a = reserves(&u, a);
        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(4, 0));
        // 9 mutations into an 8-capacity overlay (3 pools cycled) — staging overflows.
        for i in 0..9u128 {
            let pool = [a, b, c][(i % 3) as usize];
            overlay.push_mutation(v2_mutation(pool, transitional(4, i as u32), 100 + i, 100 + i));
        }
        assert!(
            overlay.overflowed(),
            "staging past MAX_DIRTY_POOLS must MARK the overlay overflowed — silently \
             dropping the ninth mutation makes the touch set a lie"
        );
        let result = publisher.publish_transaction(&mut txn, overlay);
        assert_eq!(
            result,
            Err(PublishError::CapacityExceeded),
            "an overflowed overlay must refuse ENTIRELY (typed taint + reconciliation)"
        );
        assert_eq!(
            reserves(&u, a),
            before_a,
            "no prefix of an overflowed overlay may reach the family stores"
        );
    }

    /// D5/§3.3: a batch whose per-mutation canonical points COMPLETED in reverse order still
    /// commits in canonical point order — the final store value is the NEWEST point's write.
    #[test]
    fn same_batch_reverse_completion_still_commits_canonical_transaction_order() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Arrival order: block 1002, 1000, 1001 — canonical order: 1000, 1001, 1002.
        let mut batch = StateMutationBatch {
            origin: BarrierOriginClass::Receipt,
            mutations: ArrayVec::new(),
            poison: ArrayVec::new(),
            clear_soft: ArrayVec::new(),
        };
        for (block, r) in [(1002u64, 42u128), (1000, 40), (1001, 41)] {
            batch
                .mutations
                .try_push(v2_mutation(a, canonical_point(block, 0x10), r, r))
                .expect("push");
        }
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply batch");
        assert_eq!(
            reserves(&u, a),
            (42, 42),
            "the batch must commit in CANONICAL point order — the newest canonical point \
             (block 1002) must be the final store value even though it ARRIVED first"
        );
    }

    /// F3 (codex item 5) RED→GREEN: two back-to-back full-slot writes to the SAME pool, both
    /// computed from the same resident generation and queued before one drain. The publisher
    /// applies the first (generation matches) and REFUSES the second as stale-over-new (its
    /// expected generation no longer matches) — fail-CLOSED: the pool ends POISONED-pending-resync
    /// with the first delta intact and a reconciliation scheduled, NEVER silently overwritten by
    /// the stale second value. Before the check, the second write applied and silently LOST the
    /// first delta (the WSS back-to-back-delta bug).
    #[test]
    fn back_to_back_same_pool_full_slot_writes_never_silently_lose_a_delta() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let sender = publisher.command_sender();
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Both off-thread producers observe the SAME resident generation g (nothing has committed
        // yet), exactly as two WSS logs snapshotting the same slot before either drains.
        let g = u.pools.generation(a);
        let mk = |r0, r1| {
            StateMutationBatch::single(
                BarrierOriginClass::WssLog,
                PoolStateMutation {
                    pool: a,
                    point: transitional(1, 0),
                    expected_prior_generation: g,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(r0, r1)),
                },
            )
        };
        sender.submit_batch(mk(100, 100)).expect("first fits");
        sender.submit_batch(mk(200, 200)).expect("second fits");

        let pending_before = publisher.authority().pending_reconciliations();
        publisher.drain_commands_each(&mut txn, |_| {});

        assert_eq!(
            reserves(&u, a),
            (100, 100),
            "the stale second full-slot write silently overwrote the first — the intervening \
             delta was LOST (codex item 5)"
        );
        assert_ne!(
            u.pools.param_poison(a),
            crate::pool::PARAM_POISON_NONE,
            "a refused stale-over-new write must leave the pool POISONED-pending-resync, not live"
        );
        assert!(
            publisher.authority().pending_reconciliations() > pending_before,
            "a refused stale-over-new write must schedule a reconciliation"
        );
    }

    /// WI-06 F3 fix-round-2 (codex item 4-residual, part b): a stale-over-new REFUSAL must RECORD
    /// the refused pool for targeted resync — not only bump a bare reconciliation counter. Otherwise
    /// the state thread's `service_reconciliation_obligations` consumes the counter but returns an
    /// empty resync set, and the poisoned pool is never targeted for re-snapshot.
    #[test]
    fn stale_over_new_refusal_records_the_pool_for_targeted_resync() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let sender = publisher.command_sender();
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();
        let g = u.pools.generation(a);
        let mk = |r0, r1| {
            StateMutationBatch::single(
                BarrierOriginClass::WssLog,
                PoolStateMutation {
                    pool: a,
                    point: transitional(1, 0),
                    expected_prior_generation: g,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(r0, r1)),
                },
            )
        };
        sender.submit_batch(mk(100, 100)).expect("first fits");
        sender.submit_batch(mk(200, 200)).expect("second fits");
        publisher.drain_commands_each(&mut txn, |_| {});
        // The stale second write was refused (pool poisoned); it must ALSO be recorded for resync.
        let resync = publisher.service_reconciliation_obligations(&mut txn);
        assert!(
            resync.contains(&a),
            "a stale-over-new refusal must RECORD the pool for targeted resync (not just a counter)"
        );
    }

    /// WI-06 T3 fix round 4 (codex round-4 item 5) RED→GREEN: the SHADOW stale-BLESS interleaving,
    /// isolated to the DEFENSE-IN-DEPTH `pre_replay_seq` guard. A shadow promotion computes a full-slot
    /// write from a REVM fork captured BEFORE a resident feed commit C, but its
    /// `expected_prior_generation` is stamped AFTER the replay (pool identity is a replay OUTPUT), so C
    /// — landing DURING the replay — is ABSORBED by that late stamp and the F3 generation check is
    /// DEFEATED (generation == expected_prior_generation at apply). Here the shadow's fork pre-image is
    /// contrived to still MATCH resident (so the round-5 value-parity gate PASSES and does not
    /// short-circuit), isolating the `pre_replay_seq` guard: because the pool's `last_commit_seq`
    /// advanced strictly past the shadow's captured `pre_replay_seq`, resident state KEEPS C's value.
    /// (The value-parity gate is exercised on a genuine stale-fork pre-image in the round-5 tests.)
    #[test]
    fn shadow_stale_fork_write_is_refused_when_a_commit_lands_during_replay() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // The shadow captures the GLOBAL commit sequence BEFORE its replay (before any pool identity
        // is known — the touched pool is a replay output).
        let pre_replay_seq = u.pools.commit_seq();
        // The replay reconstructs a STALE-fork value from that pre-commit fork.
        let shadow_stale = (500u128, 500u128);

        // Resident commit C lands DURING the replay: a feed delta commits a fresher value to the pool.
        let c_val = (900u128, 900u128);
        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(1, 0));
        overlay.push_mutation(v2_mutation(a, transitional(1, 0), c_val.0, c_val.1));
        publisher.publish_transaction(&mut txn, overlay).expect("commit C");
        assert!(
            u.pools.last_commit_seq(a) > pre_replay_seq,
            "commit C must advance the pool's commit sequence past the shadow's pre-replay capture"
        );

        // The shadow submits its stale-fork write. Its `expected_prior_generation` is stamped AFTER
        // the replay — i.e. NOW, post-C — exactly the codex interleaving that DEFEATS the F3 check.
        let post_c_generation = u.pools.generation(a);
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool: a,
                point: transitional(1, 0),
                expected_prior_generation: post_c_generation,
                pre_replay_seq,
                // Contrived to MATCH resident (== C) so the value-parity gate passes and this test
                // isolates the `pre_replay_seq` defense-in-depth guard as the refusing check.
                shadow_pre_image: Some(FamilyWrite::V2(v2(c_val.0, c_val.1))),
                write: FamilyWrite::V2(v2(shadow_stale.0, shadow_stale.1)),
            },
        );
        // Sanity: the F3 generation check ALONE would pass this write (it is defeated by the late stamp).
        assert_eq!(
            u.pools.generation(a),
            post_c_generation,
            "the shadow's post-replay generation stamp equals the resident generation ⇒ F3 is defeated"
        );

        let pending_before = publisher.authority().pending_reconciliations();
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");

        assert_eq!(
            reserves(&u, a),
            c_val,
            "the shadow stale-fork write BLESSED a value computed on a fork that predates commit C — \
             the pre-replay-seq guard must REFUSE it and keep C's committed value (codex round-4 item 5)"
        );
        // A seq-refused shadow write must NOT poison the pool — resident C is authoritative and fresher,
        // not stale — but the moved pool is recorded for a targeted resync (the publisher-side
        // realization of the shadow's own parity/resync fallback).
        assert_eq!(
            u.pools.param_poison(a),
            crate::pool::PARAM_POISON_NONE,
            "a seq-refused shadow write must NOT poison the pool (resident C is fresher and correct)"
        );
        assert!(
            publisher.authority().pending_reconciliations() > pending_before,
            "a seq-refused shadow write must record the pool for a targeted resync (parity fallback)"
        );
    }

    /// WI-06 T3 fix round 4 (codex round-4 item 5): NO FALSE-REFUSAL. When NOTHING commits during the
    /// replay (the pool's `last_commit_seq` does not advance past the captured `pre_replay_seq`), the
    /// shadow promotion is a valid RPC-saving optimization and MUST apply — the guard is strict (`>`),
    /// so a pool whose last commit == the captured sequence is NOT refused.
    #[test]
    fn shadow_write_applies_when_no_commit_lands_during_replay() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // A prior commit set the pool's value; the shadow captures the sequence AFTER it.
        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(1, 0));
        overlay.push_mutation(v2_mutation(a, transitional(1, 0), 400, 400));
        publisher.publish_transaction(&mut txn, overlay).expect("prior commit");

        let pre_replay_seq = u.pools.commit_seq();
        assert_eq!(
            u.pools.last_commit_seq(a),
            pre_replay_seq,
            "the pool's last commit == the captured sequence (nothing commits during replay)"
        );

        // Nothing commits during the replay; the shadow submits its (fresh) reconstruction.
        let g = u.pools.generation(a);
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool: a,
                point: transitional(1, 0),
                expected_prior_generation: g,
                pre_replay_seq,
                // The fork read the CURRENT resident (400, 400) — the pre-image matches, so the
                // value-parity gate passes and the fresh promotion applies.
                shadow_pre_image: Some(FamilyWrite::V2(v2(400, 400))),
                write: FamilyWrite::V2(v2(777, 777)),
            },
        );
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");
        assert_eq!(
            reserves(&u, a),
            (777, 777),
            "no commit landed during replay (last_commit_seq == pre_replay_seq, not >) — the shadow \
             promotion must APPLY, never be spuriously refused"
        );
        assert_eq!(
            u.pools.param_poison(a),
            crate::pool::PARAM_POISON_NONE,
            "an applied shadow write leaves the pool un-poisoned"
        );
    }

    /// WI-06 T3 fix round 5 (codex round-5 item 5) RED→GREEN — interleaving (b), the LAGGING FORK the
    /// `pre_replay_seq` guard CANNOT close. A resident commit C completes; the shadow captures its
    /// `pre_replay_seq` AFTER C (so `last_commit_seq == pre_replay_seq`, NOT `>` — the strict-`>` seq
    /// guard is satisfied), yet its REVM fork was pinned BEFORE C, so the reconstruction is computed
    /// from the PRE-C state. The stale write would clobber C's fresher value; only the value-parity
    /// (CAS) gate refuses it — the carried fork PRE-IMAGE (pre-C reserves) no longer matches the
    /// current resident (post-C). Before the gate this stale write was BLESSED.
    #[test]
    fn shadow_lagging_fork_write_is_refused_by_value_parity_when_seq_does_not_advance() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Resident commit C completes: the pool now holds (900, 900).
        let c_val = (900u128, 900u128);
        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(1, 0));
        overlay.push_mutation(v2_mutation(a, transitional(1, 0), c_val.0, c_val.1));
        publisher.publish_transaction(&mut txn, overlay).expect("commit C");

        // The shadow captures its pre-replay sequence AFTER C — the fork simply LAGS C. The strict-`>`
        // seq guard is defeated: last_commit_seq == pre_replay_seq.
        let pre_replay_seq = u.pools.commit_seq();
        assert_eq!(
            u.pools.last_commit_seq(a),
            pre_replay_seq,
            "lagging-fork case: last_commit_seq == pre_replay_seq, so the strict-> seq guard cannot fire"
        );

        // The stale reconstruction: computed from the PRE-C fork reserves (500, 500). Its
        // `expected_prior_generation` is stamped post-replay (F3 defeated), and it carries the fork
        // PRE-IMAGE it was computed FROM — pre-C (500, 500), which no longer matches resident (900, 900).
        let g = u.pools.generation(a);
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool: a,
                point: transitional(1, 0),
                expected_prior_generation: g,
                pre_replay_seq,
                shadow_pre_image: Some(FamilyWrite::V2(v2(500, 500))),
                write: FamilyWrite::V2(v2(480, 480)),
            },
        );
        let pending_before = publisher.authority().pending_reconciliations();
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");

        assert_eq!(
            reserves(&u, a),
            c_val,
            "value-parity must REFUSE: the fork pre-image (500,500) != resident (900,900) ⇒ the stale \
             lagging-fork reconstruction must NOT clobber C (codex round-5 item 5, interleaving b)"
        );
        assert_eq!(
            u.pools.param_poison(a),
            crate::pool::PARAM_POISON_NONE,
            "a value-parity refusal must NOT poison (resident is fresher/diverged, not a lost delta)"
        );
        assert!(
            publisher.authority().pending_reconciliations() > pending_before,
            "a value-parity refusal must record the pool for a targeted resync (parity/resync fallback)"
        );
    }

    /// WI-06 T3 fix round 5 (codex round-5 item 5) RED→GREEN — interleaving (a), the commit CAPTURED
    /// mid-odd-window. The shadow captures `pre_replay_seq == N` inside the writer window and commit C
    /// closes ending at the SAME `last_commit_seq == N`, so the strict-`>` seq guard is defeated and
    /// the post-replay generation stamp defeats F3. The fork still read PRE-C state, so its carried
    /// pre-image cannot match the post-C resident — the value-parity gate refuses. This is the exact
    /// interleaving codex proved the seq guard cannot see; here it is publisher-observable as
    /// `last_commit_seq == pre_replay_seq` with a mismatched pre-image.
    #[test]
    fn shadow_captured_in_window_write_is_refused_by_value_parity_when_seq_matches() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Two prior commits, then C — establishing a non-zero sequence. C brings resident to (700, 300).
        let mut o1 = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(1, 0));
        o1.push_mutation(v2_mutation(a, transitional(1, 0), 111, 111));
        publisher.publish_transaction(&mut txn, o1).expect("prior");
        let c_val = (700u128, 300u128);
        let mut o2 = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(2, 0));
        o2.push_mutation(v2_mutation(a, transitional(2, 0), c_val.0, c_val.1));
        publisher.publish_transaction(&mut txn, o2).expect("commit C");

        // The shadow's captured seq equals C's committed seq (captured N inside the window; C ended at
        // N) — the seq guard sees last == pre and does NOT fire.
        let pre_replay_seq = u.pools.commit_seq();
        assert_eq!(u.pools.last_commit_seq(a), pre_replay_seq);

        // The stale reconstruction carries the PRE-C pre-image (222, 222) it was computed from — which
        // no longer matches resident (700, 300).
        let g = u.pools.generation(a);
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool: a,
                point: transitional(2, 0),
                expected_prior_generation: g,
                pre_replay_seq,
                shadow_pre_image: Some(FamilyWrite::V2(v2(222, 222))),
                write: FamilyWrite::V2(v2(215, 229)),
            },
        );
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");

        assert_eq!(
            reserves(&u, a),
            c_val,
            "value-parity must REFUSE the mid-window-captured stale write (pre-image 222,222 != \
             resident 700,300) ⇒ resident keeps C (codex round-5 item 5, interleaving a)"
        );
        assert_eq!(u.pools.param_poison(a), crate::pool::PARAM_POISON_NONE);
    }

    /// WI-06 T3 fix round 6 (K2 part b): an ACCEPTED shadow V3 promotion PRESERVES the resident
    /// `initialised_ticks` (the warm tick window) — it merges the swap-load-bearing scalar head only,
    /// never regressing the tick set to whatever the promotion carried. The tick set is not
    /// fork-derived and a shadow V3 promotion is only emitted for a NO-tick-cross swap (which does not
    /// move it), so the freshest resident window must be carried forward, not replaced. Here the
    /// carried write deliberately holds an EMPTY tick set to prove it is ignored.
    #[test]
    fn shadow_v3_write_preserves_resident_initialised_ticks() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let resident_ticks = v3_resident(&u, pool).initialised_ticks.clone();
        assert_eq!(resident_ticks.len(), 2, "fixture pool has a 2-tick warm window");

        // A valid in-tick shadow promotion: the fork pre-image MATCHES resident (CAS passes) and the
        // sqrt price moves within the current tick (tick UNCHANGED, STRICTLY-INTERIOR landing — no-cross
        // under the L1/M1/N3 predicate). The carried write's tick set is EMPTY — it must NOT reach the store.
        let pre = v3_resident(&u, pool);
        let interior = engine_amm::scythe::sqrt_ratio_at_tick(100).expect("sqrt(100)")
            + alloy_primitives::U256::from(1u8);
        let mut write = pre.clone();
        write.sqrt_price_x96 = interior;
        write.initialised_ticks = Vec::new();
        let g = u.pools.generation(pool);
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool,
                point: transitional(1, 0),
                expected_prior_generation: g,
                pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                shadow_pre_image: Some(FamilyWrite::V3(pre.clone())),
                write: FamilyWrite::V3(write),
            },
        );
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.sqrt_price_x96, interior,
            "the swap-load-bearing scalar head IS applied"
        );
        assert_eq!(after.tick, 100, "tick UNCHANGED (in-tick swap) — the merge keeps the resident tick");
        assert_eq!(
            after.initialised_ticks, resident_ticks,
            "K2 part b: the resident tick window MUST be preserved (merge), not regressed to the \
             write's (here empty) set"
        );
    }

    /// WI-06 T3 fix round 7 (L2): an ACCEPTED shadow V3 promotion PRESERVES the resident
    /// `tick_spacing` — exactly like `initialised_ticks`. `tick_spacing` is a resident-seeded static
    /// field (the reconstruction copies it from the base), NOT fork-derived, and it is NOT in the
    /// value-parity compare tuple, so the merge must never REPLACE it with the promotion-time value
    /// (which would silently regress a genuinely-fresher resident spacing — e.g. Camelot's
    /// admin-mutable spacing). Here the carried write holds a deliberately-wrong `tick_spacing`; the
    /// resident's must survive. Pre-L2 the merge assigned `merged.tick_spacing = new_state.tick_spacing`.
    #[test]
    fn shadow_v3_write_preserves_resident_tick_spacing() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let resident_spacing = v3_resident(&u, pool).tick_spacing;
        assert_eq!(resident_spacing, 60, "fixture pool has tick_spacing 60");

        // A valid tick-UNCHANGED shadow promotion (only sqrt moves within the current tick, a
        // STRICTLY-INTERIOR landing — no-cross under the L1/M1/N3 predicate): the fork pre-image MATCHES
        // resident (CAS passes). The carried write holds a bogus tick_spacing (999) — it must NOT reach
        // the store.
        let pre = v3_resident(&u, pool);
        let interior = engine_amm::scythe::sqrt_ratio_at_tick(100).expect("sqrt(100)")
            + alloy_primitives::U256::from(1u8);
        let mut write = pre.clone();
        write.sqrt_price_x96 = interior;
        write.tick_spacing = 999;
        let g = u.pools.generation(pool);
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool,
                point: transitional(1, 0),
                expected_prior_generation: g,
                pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                shadow_pre_image: Some(FamilyWrite::V3(pre.clone())),
                write: FamilyWrite::V3(write),
            },
        );
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.sqrt_price_x96, interior,
            "the swap-load-bearing scalar head IS applied"
        );
        assert_eq!(
            after.tick_spacing, resident_spacing,
            "L2: the resident tick_spacing MUST be preserved (merge), not replaced by the write's (bogus) value"
        );
    }

    /// WI-06 T3 fix round 6 (K3): a fee-only resident divergence (the dynamic Algebra fee the fork
    /// LAGS) — sqrt/tick/liquidity all match, only `fee_pips` differs — MUST be refused by the CAS,
    /// because fee is a direct quote input and a V3 shadow write full-replaces the fee. Pre-K3 the
    /// matcher ignored fee, so the stale write was blessed.
    #[test]
    fn shadow_v3_fee_only_divergence_is_refused_by_value_parity() {
        let (u, pool) = test_universe_v3(); // fixture fee = 3000
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Resident took an authoritative fee-only commit: fee 3000 -> 500, scalars unchanged.
        let mut fee_commit = v3_resident(&u, pool);
        fee_commit.fee_pips = 500;
        let g0 = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::FeedTrigger,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g0,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: None,
                        write: FamilyWrite::V3(fee_commit),
                    },
                ),
            )
            .expect("commit fee-only change");
        assert_eq!(v3_resident(&u, pool).fee_pips, 500, "resident now holds the fresher fee");

        // A stale shadow promotion whose fork pre-image still holds fee 3000 (lagging the fee commit),
        // with IDENTICAL sqrt/tick/liquidity. The scalar head it would write is (correctly) fresh, but
        // its fee is stale — the CAS must refuse on the fee term alone.
        let mut pre = v3_resident(&u, pool);
        pre.fee_pips = 3_000;
        let mut write = v3_resident(&u, pool);
        write.sqrt_price_x96 = alloy_primitives::U256::from(1_050_000u128);
        write.fee_pips = 3_000;
        let g1 = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(2, 0),
                        expected_prior_generation: g1,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(after.fee_pips, 500, "fee-only divergence must be REFUSED — resident keeps fee 500");
        assert_eq!(
            after.sqrt_price_x96,
            alloy_primitives::U256::from(1_000_000u128),
            "the stale shadow write must NOT apply (its scalar head must not land)"
        );
    }

    /// WI-06 T3 fix round 6 (K5): an accepted shadow V2 write updates the reserves ONLY and PRESERVES
    /// the resident `block_timestamp_last`. The shadow reconstruction forces bt=0 (its packed-slot
    /// match is timestamp-agnostic), so a full replace would REGRESS a field detection reads, WSS
    /// carries, and the resident/prediction hashes read. Pre-K5 the V2 apply full-replaced, zeroing it.
    #[test]
    fn shadow_v2_write_preserves_resident_block_timestamp_last() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Seed a non-zero resident block_timestamp_last via an authoritative (non-shadow) write.
        let seeded = V2State {
            reserves: V2Reserves256 {
                reserve0: 500,
                reserve1: 500,
                block_timestamp_last: 123_456,
            },
        };
        let g0 = u.pools.generation(a);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::FeedTrigger,
                    PoolStateMutation {
                        pool: a,
                        point: transitional(1, 0),
                        expected_prior_generation: g0,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: None,
                        write: FamilyWrite::V2(seeded),
                    },
                ),
            )
            .expect("seed bt_last");
        let slot = u.pools.meta(a).state_slot;
        assert_eq!(
            u.v2_state.snapshot(slot).reserves.block_timestamp_last,
            123_456
        );

        // An accepted shadow write: pre-image matches resident reserves (500,500); the write moves the
        // reserves to (480,480) with bt forced to 0 (exactly as the reconstruction produces it).
        let g1 = u.pools.generation(a);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool: a,
                        point: transitional(2, 0),
                        expected_prior_generation: g1,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V2(v2(500, 500))),
                        write: FamilyWrite::V2(v2(480, 480)),
                    },
                ),
            )
            .expect("apply shadow V2 write");

        let after = u.v2_state.snapshot(slot);
        assert_eq!(
            (after.reserves.reserve0, after.reserves.reserve1),
            (480, 480),
            "the reserves (the fork-derived, CAS-compared fields) ARE applied"
        );
        assert_eq!(
            after.reserves.block_timestamp_last, 123_456,
            "K5: the resident block_timestamp_last MUST be preserved (not zeroed by a full replace)"
        );
    }

    /// WI-06 T3 fix round 8 (M2) — codex round-8 NEW defect 1. A SHADOW DODO promotion MERGES: it
    /// writes the compared PMM/quote fields (reserves, i/k, targets, r_state, fees) and PRESERVES the
    /// resident `block_number`/`log_index`. The shadow reconstruction decodes pre/post at
    /// `block_number = 0`/`log_index = 0` (it cannot observe the post-swap stamps), and the receipt
    /// reconciliation reads FULL `DodoState` equality, so a full replace would REGRESS a resident
    /// nonzero stamp to zero and change `changed`/generation-bump/wake behavior. Pre-M2 the Dodo apply
    /// full-replaced even for Shadow origin.
    #[test]
    fn shadow_dodo_write_preserves_resident_block_stamps() {
        // Resident holds nonzero provenance stamps (block 12, log 7) and reserves (100, 200).
        let (u, pool) = test_universe_dodo(dodo(100, 200, 12, 7));
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let resident_snap = dodo_resident(&u, pool);
        assert_eq!((resident_snap.block_number, resident_snap.log_index), (12, 7));

        // Shadow pre-image: matches resident on the compared PMM fields; stamps decoded at 0 (NOT
        // compared, so the CAS still passes). The write is the post-swap state — reserves moved to
        // (110, 190), stamps again 0 (the shadow decode).
        let mut pre = resident_snap.clone();
        pre.block_number = 0;
        pre.log_index = 0;
        let mut write = resident_snap.clone();
        write.base_reserve = alloy_primitives::U256::from(110u64);
        write.quote_reserve = alloy_primitives::U256::from(190u64);
        write.block_number = 0;
        write.log_index = 0;

        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::Dodo(Box::new(pre))),
                        write: FamilyWrite::Dodo(Box::new(write)),
                    },
                ),
            )
            .expect("apply shadow dodo write");

        let after = dodo_resident(&u, pool);
        assert_eq!(
            after.base_reserve,
            alloy_primitives::U256::from(110u64),
            "the swap-load-bearing base_reserve IS applied (merge)"
        );
        assert_eq!(after.quote_reserve, alloy_primitives::U256::from(190u64), "quote_reserve applied");
        assert_eq!(
            (after.block_number, after.log_index),
            (12, 7),
            "M2: the resident block_number/log_index MUST be preserved (not regressed to 0 by a full \
             replace) — receipt-equality reconciliation reads the full DodoState"
        );
        // Strong check: the merge changed ONLY the two compared reserves; every other field
        // (identity, i/k, targets, r_state, fees, stamps) rode the resident snapshot.
        let mut expected = resident_snap.clone();
        expected.base_reserve = alloy_primitives::U256::from(110u64);
        expected.quote_reserve = alloy_primitives::U256::from(190u64);
        assert_eq!(after, expected, "merge preserves every non-swap field");
    }

    /// WI-06 T3 fix round 9 (N4) — codex round-9 defect 2. `fee_ppm` is ASSIGNED by the Dodo merge but
    /// NOT compared by the value-parity CAS (which compares `lp_fee_rate_d18`/`mt_fee_rate_d18`), so a
    /// malformed Shadow batch could otherwise land an `fee_ppm` inconsistent with its (CAS-matched)
    /// rates. The merge must DERIVE `fee_ppm` from the two compared rates (`fee_rate_to_ppm(lp + mt)` —
    /// the invariant every normal Dodo producer maintains: `dodo_v2::apply_swap` /
    /// `dodo_state_from_view`), never trust the batch value. Here the write carries the resident's exact
    /// rates but a bogus `fee_ppm`; the derived value (4000) must land, not the bogus one.
    #[test]
    fn shadow_dodo_fee_ppm_is_derived_not_trusted_from_the_batch() {
        let (u, pool) = test_universe_dodo(dodo(100, 200, 12, 7));
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let resident_snap = dodo_resident(&u, pool);
        // The correct fee_ppm derived from the resident's rates (3e15 + 1e15 = 4e15 ⇒ 4000 ppm).
        let derived = engine_amm::dodo_v2::fee_rate_to_ppm(
            resident_snap.lp_fee_rate_d18 + resident_snap.mt_fee_rate_d18,
        )
        .expect("derive fee_ppm");
        assert_eq!(derived, 4_000, "sanity: 4e15 total fee rate ⇒ 4000 ppm");
        assert_eq!(resident_snap.fee_ppm, derived, "resident fee_ppm is consistent with its rates");

        // Pre-image matches resident on the compared fields (stamps at 0 — not compared). The write
        // carries the resident's EXACT rates but a BOGUS fee_ppm (999_999) that disagrees with them —
        // fee_ppm is not in the CAS tuple, so the batch would otherwise pass and land the bogus value.
        let mut pre = resident_snap.clone();
        pre.block_number = 0;
        pre.log_index = 0;
        let mut write = resident_snap.clone();
        write.fee_ppm = 999_999; // MALFORMED — inconsistent with lp_fee_rate_d18 + mt_fee_rate_d18
        write.block_number = 0;
        write.log_index = 0;

        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::Dodo(Box::new(pre))),
                        write: FamilyWrite::Dodo(Box::new(write)),
                    },
                ),
            )
            .expect("apply shadow dodo write");

        let after = dodo_resident(&u, pool);
        assert_eq!(
            after.fee_ppm, derived,
            "N4: the Dodo merge must DERIVE fee_ppm from the two compared rates, never trust the \
             batch's (here malformed) fee_ppm"
        );
        assert_ne!(after.fee_ppm, 999_999, "the malformed batch fee_ppm must NOT land");
    }

    /// WI-06 T3 fix round 8 (M3) — codex round-8 NEW defect 2 (shape-partial fail-closed default). The
    /// V3 tick/boundary gate runs only under `(FamilyWrite::V3, FamilyWrite::V3)`, `family_scope_ok`
    /// validates only the WRITE against the pool kind, and resident selection keys off the PRE-IMAGE
    /// variant — so a malformed Shadow batch pairing a V2 pre-image with a V3 write would skip the V3
    /// gate entirely. This builds a mixed V2+V3 universe (both take slot 0 in their per-family stores,
    /// so the V2-keyed resident snapshot at the V3 pool's slot reads the V2 pool's reserves and the CAS
    /// can pass), then submits a V2-pre-image / V3-write batch. The shape check must REFUSE it before
    /// the family gates. Pre-M3 the old code applied the V3 write (V3 gate skipped).
    #[test]
    fn shadow_mismatched_preimage_write_shape_is_refused() {
        let mut b = UniverseBuilder::new();
        let t0 = b.add_token([0x11; 20], 18, RiskClass::Permissionless);
        let t1 = b.add_token([0x22; 20], 18, RiskClass::Permissionless);
        let _v2_pool = b
            .add_v2_pool([0xA0; 20], PoolKind::UniswapV2, t0, t1, VenueId(0), 30, v2(500, 500))
            .expect("add v2 pool");
        let v3_pool = b
            .add_v3_pool(
                [0xB3; 20],
                PoolKind::UniswapV3,
                t0,
                t1,
                VenueId(1),
                3_000,
                60,
                v3(1_000_000, 100, 5_000_000, 3_000, vec![tick_info(-60, 111), tick_info(120, -111)]),
            )
            .expect("add v3 pool");
        let u = StdArc::new(b.build());
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let v3_slot = u.pools.meta(v3_pool).state_slot;
        // The V2 state resident at the V3 pool's slot index (the V2 pool's reserves). The mismatched
        // pre-image is crafted to MATCH it so the CAS would pass on the old (shape-blind) code.
        let v2_at_slot = u.v2_state.snapshot(v3_slot);
        let pre = FamilyWrite::V2(v2_at_slot);

        let before = v3_resident(&u, v3_pool);
        let mut write_v3 = before.clone();
        write_v3.sqrt_price_x96 = alloy_primitives::U256::from(1_050_000u128);

        let g = u.pools.generation(v3_pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool: v3_pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(pre), // V2 pre-image ...
                        write: FamilyWrite::V3(write_v3), // ... paired with a V3 write (shape mismatch)
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, v3_pool);
        assert_eq!(
            after.sqrt_price_x96, before.sqrt_price_x96,
            "M3: a Shadow batch whose pre-image variant (V2) does not match the write variant (V3) MUST \
             be refused (fail-closed shape check) — the V3 tick/boundary gate must not be skipped"
        );
    }

    /// WI-06 T3 fix round 7 (L1) — codex round-7 counterexample 1 (two-cross net-final-zero L). A
    /// lagging fork holds two ticks the resident BURNED; one monotone swap crosses both, active L
    /// differs between them but returns to its original value after the second cross (so slot 4 is
    /// final-equal and vanishes from the change set — K2a cannot see it), and the fork's post tick
    /// lands BEYOND both burned bounds. The resident window no longer holds those ticks, so the
    /// (now-deleted) traversed-range gate reported EMPTY and blessed the overshoot. Under the uniform
    /// rule the tick MOVED (100 -> 300) ⇒ REFUSED.
    #[test]
    fn shadow_univ3_two_cross_net_zero_liquidity_tick_move_is_refused() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Model "resident burned the crossed ticks": an authoritative resync leaves an EMPTY warm
        // window, tick unchanged at 100 (a full-replace non-shadow write).
        let mut burned = v3_resident(&u, pool);
        burned.initialised_ticks = Vec::new();
        let g0 = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::FeedTrigger,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g0,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: None,
                        write: FamilyWrite::V3(burned),
                    },
                ),
            )
            .expect("burn ticks (empty window)");

        // pre-image == current resident scalars (value CAS passes); the write moves tick 100 -> 300
        // with active L returned to its original value (net-final-zero: the two-cross round trip).
        let pre = v3_resident(&u, pool);
        let mut write = pre.clone();
        write.sqrt_price_x96 = alloy_primitives::U256::from(3_000_000u128);
        write.tick = 300;
        let g1 = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(2, 0),
                        expected_prior_generation: g1,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre.clone())),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.tick, 100,
            "uniform rule: a tick-MOVING UniV3 promotion (net-zero-L two-cross) MUST be refused — the \
             fork's overshoot post-price must not land"
        );
        assert_eq!(after.sqrt_price_x96, pre.sqrt_price_x96);
    }

    /// WI-06 T3 fix round 7 (L1) — codex round-7 counterexample 2 (downward exact-destination). A
    /// zero-for-one swap ends EXACTLY at sqrt(T) for a resident initialized tick T the fork missed; the
    /// fork does not cross and stores post tick T, while the true chain crosses and stores T-1. The
    /// (now-deleted) traversed-range gate made T the EXCLUDED low endpoint of (T, pre] and returned
    /// safe. Under the uniform rule the tick MOVED (100 -> -60) ⇒ REFUSED. (Fixture resident holds an
    /// initialized tick at -60.)
    #[test]
    fn shadow_univ3_downward_exact_destination_tick_move_is_refused() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        assert!(
            before.initialised_ticks.iter().any(|t| t.tick == -60),
            "fixture resident holds an initialized tick at the exact destination -60"
        );

        // pre-image == resident scalars (value CAS passes); the write is the fork's exact-destination
        // no-cross record: post tick T == -60 (the excluded low endpoint the deleted gate blessed).
        let pre = before.clone();
        let mut write = before.clone();
        write.sqrt_price_x96 = alloy_primitives::U256::from(60_000u128);
        write.tick = -60;
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.tick, 100,
            "uniform rule: a downward exact-destination tick move MUST be refused (the fork stored the \
             boundary tick the true chain crossed past)"
        );
        assert_eq!(after.sqrt_price_x96, before.sqrt_price_x96);
    }

    /// WI-06 T3 fix round 7 (L1) — codex round-7 counterexample 3 (out-of-window resident mint). The
    /// resident warm window is a bounded ±width fetch, so a mint OUTSIDE it is invisible to the
    /// (now-deleted) traversed-range gate — absence from the Vec is not canonical absence. A fork
    /// pinned before the mint replays a tick-moving traversal the gate blessed as empty. Under the
    /// uniform rule the tick MOVED (100 -> 200) ⇒ REFUSED regardless of window coverage.
    #[test]
    fn shadow_univ3_out_of_window_resident_mint_tick_move_is_refused() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Model an out-of-window resident: the bounded warm window holds NO tick in the traversed range.
        let mut narrow = v3_resident(&u, pool);
        narrow.initialised_ticks = Vec::new();
        let g0 = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::FeedTrigger,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g0,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: None,
                        write: FamilyWrite::V3(narrow),
                    },
                ),
            )
            .expect("narrow window (out-of-window mint invisible)");

        // pre-image == resident scalars (value CAS passes); the write moves tick 100 -> 200 with L
        // UNCHANGED — a fork that MISSED the out-of-window mint retains its active L (it crossed no tick
        // in its own bitmap). The tick MOVE alone triggers the refusal; a fork slot-4 L change would
        // already be declined by the builder's belt (codex round-8 §3 fixture-fidelity fix).
        let pre = v3_resident(&u, pool);
        let mut write = pre.clone();
        write.sqrt_price_x96 = alloy_primitives::U256::from(2_000_000u128);
        write.tick = 200;
        write.liquidity = pre.liquidity;
        let g1 = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(2, 0),
                        expected_prior_generation: g1,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre.clone())),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.tick, 100,
            "uniform rule: an out-of-window mint tick move MUST be refused — warm-window coverage is \
             irrelevant to the pre.tick == post.tick gate"
        );
        assert_eq!(after.sqrt_price_x96, pre.sqrt_price_x96);
    }

    /// WI-06 T3 fix round 7 (L1) — the uniform-rule equivalent of the deleted traversed-range boundary
    /// test: even the SMALLEST tick move (a single tick 100 -> 101, whose traversed range held no
    /// resident tick and which the deleted gate APPLIED) is now REFUSED. Any tick change fails the
    /// pre.tick == post.tick gate; only a tick-unchanged swap can promote.
    #[test]
    fn shadow_univ3_adjacent_single_tick_move_is_refused() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        let pre = before.clone();
        let mut write = before.clone();
        write.sqrt_price_x96 = alloy_primitives::U256::from(1_000_001u128);
        write.tick = 101;
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(after.tick, 100, "uniform rule refuses even a single-tick move");
        assert_eq!(after.sqrt_price_x96, before.sqrt_price_x96);
    }

    /// WI-06 T3 fix round 7 (L1): a UniV3 tick-MOVING promotion whose pre-image MATCHES resident
    /// scalars (value CAS passes) is refused by the uniform `pre.tick == post.tick` gate. The fixture
    /// resident has an initialized tick at 120 (current tick 100); the write moves the tick 100→200 —
    /// the case the deleted traversed-range gate caught (120 inside the range) is now subsumed by the
    /// uniform rule, which refuses the move directly without inspecting the resident tick set.
    #[test]
    fn shadow_univ3_tick_move_with_resident_tick_in_range_is_refused() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        assert!(
            before.initialised_ticks.iter().any(|t| t.tick == 120),
            "fixture resident has an initialized tick at 120"
        );

        // pre-image == resident scalars (value CAS passes); the write moves the tick 100→200 (the fork
        // replayed it as no-cross), and 120 lies inside the moved span — subsumed by the tick-move gate.
        let pre = before.clone();
        let mut write = before.clone();
        write.sqrt_price_x96 = alloy_primitives::U256::from(2_000_000u128);
        write.tick = 200;
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.tick, 100,
            "uniform rule: a tick-moving UniV3 promotion MUST be refused — the stale overshoot write \
             must not land"
        );
        assert_eq!(after.sqrt_price_x96, before.sqrt_price_x96);
    }

    /// WI-06 T3 fix round 7 (L1) companion — NO FALSE-REFUSAL for the common case: a UniV3 in-tick swap
    /// (only the sqrt price moves; the tick is UNCHANGED at 100) is provably no-cross and APPLIES,
    /// preserving the resident tick window through the merge. Proves the uniform rule loses no coverage
    /// for the overwhelmingly common in-tick promotion. WI-06 T3 fix round 9 (N3): the post sqrt is a
    /// REAL strictly-interior value of tick 100's span (`sqrt(100) + 1`) — the previous fixture used
    /// `1_050_000`, which is BELOW the engine's `MIN_SQRT_RATIO` and outside tick 100's span, so it
    /// would (correctly) be refused now; it only "passed" under the old `!= endpoints` check.
    #[test]
    fn shadow_univ3_in_tick_swap_applies() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        // Strictly inside tick 100's open span (span `sqrt(101) - sqrt(100)` is ~10^24 wide).
        let interior = engine_amm::scythe::sqrt_ratio_at_tick(100).expect("sqrt(100)")
            + alloy_primitives::U256::from(1u8);
        assert!(interior < engine_amm::scythe::sqrt_ratio_at_tick(101).expect("sqrt(101)"));
        let pre = before.clone();
        let mut write = before.clone();
        write.sqrt_price_x96 = interior;
        // tick UNCHANGED (in-tick swap).
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(after.tick, 100, "an in-tick (tick-unchanged) swap is provably no-cross and APPLIES");
        assert_eq!(after.sqrt_price_x96, interior);
        assert_eq!(
            after.initialised_ticks, before.initialised_ticks,
            "the resident tick window is preserved through the merge"
        );
    }

    /// WI-06 T3 fix round 8 (M1) — codex/Fable round-8 corner (downward closed-endpoint landing). A
    /// zero-for-one swap clamped EXACTLY at `sqrt(T)` for `T` == the current (pre) tick leaves the
    /// lagging fork's stored tick `T -> T` (tick-UNCHANGED, so the round-7 gate would pass), while a
    /// resident-only endpoint-initializing mint `[T-spacing, T]` (minted after the fork pinned) makes
    /// the true chain CROSS `T` at the same endpoint and store `T-1` with the net applied. The
    /// divergence lives AT the closed endpoint and is invisible to every compared scalar. The exact-
    /// boundary refusal (`post_sqrt == sqrt_ratio_at_tick(post_tick)`) closes it representation-
    /// independently. (Here the write's post sqrt is EXACTLY `sqrt(100)` with tick UNCHANGED at 100.)
    #[test]
    fn shadow_univ3_downward_closed_endpoint_landing_is_refused() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        // pre-image == resident scalars (value CAS passes); the write records the fork's exact-endpoint
        // no-cross: tick UNCHANGED at 100, post sqrt EXACTLY on the tick-100 boundary `sqrt(100)`.
        let pre = before.clone();
        let mut write = before.clone();
        write.sqrt_price_x96 = engine_amm::scythe::sqrt_ratio_at_tick(100).expect("sqrt(100)");
        // tick UNCHANGED (the escape the tick-unchanged gate cannot see).
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.sqrt_price_x96, before.sqrt_price_x96,
            "M1: a tick-unchanged promotion landing EXACTLY on `sqrt(post_tick)` MUST be refused — the \
             (tick, L) representation is ambiguous at the closed endpoint and a fork-lagged \
             endpoint-initializing mint is invisible to every compared scalar"
        );
        assert_eq!(after.tick, 100, "resident tick unchanged (write refused)");
    }

    /// WI-06 T3 fix round 8 (M1) — upward-direction mirror. The SAME ambiguity exists at the tick's
    /// UPPER boundary `sqrt(T+1)`: a promotion whose post sqrt lands EXACTLY there (tick still recorded
    /// as the pre tick) is a boundary representation the compared scalars cannot disambiguate. Refused
    /// by the `post_sqrt == sqrt_ratio_at_tick(post_tick + 1)` arm. (Post sqrt is EXACTLY `sqrt(101)`
    /// with tick UNCHANGED at 100.)
    #[test]
    fn shadow_univ3_upward_exact_boundary_landing_is_refused() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        let pre = before.clone();
        let mut write = before.clone();
        write.sqrt_price_x96 = engine_amm::scythe::sqrt_ratio_at_tick(101).expect("sqrt(101)");
        // tick UNCHANGED at 100 (the write records the pre tick despite landing on the upper boundary).
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.sqrt_price_x96, before.sqrt_price_x96,
            "M1: a tick-unchanged promotion landing EXACTLY on `sqrt(post_tick + 1)` MUST be refused \
             (both adjacent boundaries are ambiguity points)"
        );
        assert_eq!(after.tick, 100, "resident tick unchanged (write refused)");
    }

    /// WI-06 T3 fix round 8 (M1) companion — NO FALSE-REFUSAL for the common case. A tick-unchanged
    /// promotion whose post sqrt lands strictly INSIDE the current tick span (neither `sqrt(T)` nor
    /// `sqrt(T+1)`) is provably no-cross under the refined predicate and APPLIES. Proves the exact-
    /// boundary refusal is measure-zero: it never rejects an interior landing. (Post sqrt is
    /// `sqrt(100) + 1`, strictly interior since the span `sqrt(101) - sqrt(100)` is ~10^24 wide.)
    #[test]
    fn shadow_univ3_interior_landing_applies() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        let interior =
            engine_amm::scythe::sqrt_ratio_at_tick(100).expect("sqrt(100)") + alloy_primitives::U256::from(1u8);
        // Sanity: strictly inside the open span (not the upper boundary).
        assert!(interior < engine_amm::scythe::sqrt_ratio_at_tick(101).expect("sqrt(101)"));
        let pre = before.clone();
        let mut write = before.clone();
        write.sqrt_price_x96 = interior;
        // tick UNCHANGED at 100.
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert_eq!(
            after.sqrt_price_x96, interior,
            "M1: a strictly-interior tick-unchanged landing APPLIES (no false refusal)"
        );
        assert_eq!(after.tick, 100, "tick unchanged (in-tick swap)");
    }

    /// WI-06 T3 fix round 9 (N3) — codex round-9 defect 1: the M1 gate must enforce TRUE range
    /// membership `sqrt(post_tick) < post_sqrt < sqrt(post_tick + 1)`, not merely `!= endpoints`. A
    /// post sqrt that is NEITHER endpoint but lies OUTSIDE the stored tick's open span is an
    /// inconsistent (tick, sqrt) pair — a valid V3 slot0 never has one — and must be REFUSED. Two
    /// witnesses (both tick-UNCHANGED at 100, both non-endpoint): a sqrt just BELOW `sqrt(100)` (in
    /// tick 99's span) and a sqrt just ABOVE `sqrt(101)` (in tick 101's span). The old `!= endpoints`
    /// check BLESSED both (this is exactly the class that let the below-`MIN_SQRT_RATIO` fixture
    /// `1_050_000` land); strict-interior refuses both.
    #[test]
    fn shadow_univ3_out_of_span_non_endpoint_landing_is_refused() {
        let at_100 = engine_amm::scythe::sqrt_ratio_at_tick(100).expect("sqrt(100)");
        let at_101 = engine_amm::scythe::sqrt_ratio_at_tick(101).expect("sqrt(101)");
        for out_of_span in [
            at_100 - alloy_primitives::U256::from(1u8), // below the span (tick 99's), not an endpoint
            at_101 + alloy_primitives::U256::from(1u8), // above the span (tick 101's), not an endpoint
        ] {
            // Sanity: the witness is genuinely NEITHER adjacent boundary, so the old `!= endpoints`
            // check would have passed it — the ONLY thing refusing it is true span membership.
            assert_ne!(out_of_span, at_100);
            assert_ne!(out_of_span, at_101);

            let (u, pool) = test_universe_v3();
            let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
            let mut permit = StateWriterPermit::test_mint();
            let mut txn = permit.begin_txn();

            let before = v3_resident(&u, pool);
            assert_eq!(before.tick, 100);
            let pre = before.clone();
            let mut write = before.clone();
            write.sqrt_price_x96 = out_of_span;
            // tick UNCHANGED at 100 (the (tick, sqrt) pair is inconsistent: sqrt is outside tick 100's span).
            let g = u.pools.generation(pool);
            publisher
                .apply_mutation_batch(
                    &mut txn,
                    StateMutationBatch::single(
                        BarrierOriginClass::Shadow,
                        PoolStateMutation {
                            pool,
                            point: transitional(1, 0),
                            expected_prior_generation: g,
                            pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                            shadow_pre_image: Some(FamilyWrite::V3(pre)),
                            write: FamilyWrite::V3(write),
                        },
                    ),
                )
                .expect("apply");

            let after = v3_resident(&u, pool);
            assert_eq!(
                after.sqrt_price_x96, before.sqrt_price_x96,
                "N3: a tick-unchanged post sqrt OUTSIDE the stored tick's open span (but not an \
                 endpoint) MUST be refused — the (tick, sqrt) pair is inconsistent, so the gate must \
                 enforce sqrt(post_tick) < post_sqrt < sqrt(post_tick + 1), not merely != endpoints"
            );
            assert_eq!(after.tick, 100, "resident tick unchanged (write refused)");
        }
    }

    /// WI-06 T3 fix round 7 (L3, Fable nit 4): the shadow MERGE-vs-full-replace apply semantics key on
    /// the batch ORIGIN (Shadow), NOT on `shadow_pre_image.is_some()`. Only a Shadow-origin write — the
    /// one class the value-parity CAS gate guards — gets merge (field-preserving) semantics; every other
    /// origin full-replaces. Here a NON-Shadow (FeedTrigger) mutation carries a pre-image AND a V3 write
    /// with an EMPTY tick set; because its origin is not Shadow it must FULL-REPLACE (regressing the tick
    /// set to empty), not merge. Pre-fix the apply keyed on the pre-image presence, so this write would
    /// have silently taken merge semantics while BYPASSING the origin-scoped CAS — the latent asymmetry.
    #[test]
    fn non_shadow_origin_with_pre_image_full_replaces_not_merge() {
        let (u, pool) = test_universe_v3();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let before = v3_resident(&u, pool);
        assert_eq!(before.initialised_ticks.len(), 2, "fixture resident has a 2-tick warm window");

        let mut write = before.clone();
        write.sqrt_price_x96 = alloy_primitives::U256::from(1_200_000u128);
        write.initialised_ticks = Vec::new();
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    // NON-Shadow origin carrying a (would-be) pre-image: must full-replace, not merge.
                    BarrierOriginClass::FeedTrigger,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(before.clone())),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");

        let after = v3_resident(&u, pool);
        assert!(
            after.initialised_ticks.is_empty(),
            "L3 nit-4: a NON-Shadow write full-replaces (merge is Shadow-origin-only) — the tick set is \
             regressed to the write's empty set, not preserved"
        );
        assert_eq!(after.sqrt_price_x96, alloy_primitives::U256::from(1_200_000u128));
    }

    /// WI-06 T3 fix round 6 (K9) / round 7 (L1): the uniform rule applied to Algebra (identical for
    /// every V3-class family). A single swap is price-monotone, so a cross moves the stored tick — any
    /// tick CHANGE is refused for every V3-class family; only a tick-UNCHANGED, interior landing promotes
    /// (L1/M1). The companion proves a tick-unchanged Algebra promotion still applies (no blanket refusal).
    #[test]
    fn shadow_algebra_tick_change_is_refused_no_slot4_cross_signal() {
        let (u, pool) = test_universe_v3_kind(PoolKind::AlgebraV19);
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // tick-CHANGED promotion (pre 100 → post 105); pre-image matches resident scalars (value CAS passes).
        let before = v3_resident(&u, pool);
        assert_eq!(before.tick, 100);
        let pre = before.clone();
        let mut write = before.clone();
        write.tick = 105;
        write.sqrt_price_x96 = alloy_primitives::U256::from(1_010_000u128);
        let g = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(1, 0),
                        expected_prior_generation: g,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre)),
                        write: FamilyWrite::V3(write),
                    },
                ),
            )
            .expect("apply");
        assert_eq!(
            v3_resident(&u, pool).tick,
            100,
            "K9: a tick-CHANGED Algebra promotion must be refused (no slot-4 cross signal to prove no-cross)"
        );

        // Companion (no false-refusal): a tick-UNCHANGED swap (only sqrt moves STRICTLY INSIDE the tick's
        // open span) APPLIES. (Algebra/Kyber use the same 1.0001-base tick math, so tick 100's interior
        // is `sqrt(100)..sqrt(101)`.)
        let before2 = v3_resident(&u, pool);
        let pre2 = before2.clone();
        let interior2 = engine_amm::scythe::sqrt_ratio_at_tick(100).expect("sqrt(100)")
            + alloy_primitives::U256::from(1u8);
        let mut write2 = before2.clone();
        write2.sqrt_price_x96 = interior2;
        let g2 = u.pools.generation(pool);
        publisher
            .apply_mutation_batch(
                &mut txn,
                StateMutationBatch::single(
                    BarrierOriginClass::Shadow,
                    PoolStateMutation {
                        pool,
                        point: transitional(2, 0),
                        expected_prior_generation: g2,
                        pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                        shadow_pre_image: Some(FamilyWrite::V3(pre2)),
                        write: FamilyWrite::V3(write2),
                    },
                ),
            )
            .expect("apply");
        let after2 = v3_resident(&u, pool);
        assert_eq!(after2.tick, 100);
        assert_eq!(
            after2.sqrt_price_x96, interior2,
            "a tick-UNCHANGED Algebra swap is provably no-cross and applies (no false refusal)"
        );
    }

    /// WI-06 T3 fix round 5 (codex round-5 item 5): NO FALSE-REFUSAL for the value-parity gate. When
    /// the fork read the CURRENT resident state (its pre-image MATCHES resident) and no commit
    /// intervened, the shadow promotion is a valid RPC-saving optimization and MUST apply — the CAS
    /// succeeds. Proves the gate is precise, not a blanket refusal of shadow writes.
    #[test]
    fn shadow_write_applies_when_value_parity_pre_image_matches_resident() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Resident holds (400, 400).
        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(1, 0));
        overlay.push_mutation(v2_mutation(a, transitional(1, 0), 400, 400));
        publisher.publish_transaction(&mut txn, overlay).expect("prior");

        let pre_replay_seq = u.pools.commit_seq();
        let g = u.pools.generation(a);
        // The fork read the CURRENT resident (400, 400) — the pre-image MATCHES; the promotion is fresh.
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool: a,
                point: transitional(1, 0),
                expected_prior_generation: g,
                pre_replay_seq,
                shadow_pre_image: Some(FamilyWrite::V2(v2(400, 400))),
                write: FamilyWrite::V2(v2(555, 321)),
            },
        );
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");
        assert_eq!(
            reserves(&u, a),
            (555, 321),
            "value-parity match (fork read the current resident) ⇒ the fresh promotion must APPLY"
        );
    }

    /// WI-06 T3 fix round 5 (codex round-5 item 5): TOTALITY. A `Shadow`-origin full-slot write with NO
    /// value-parity pre-image is REFUSED (fail-closed) — a shadow promotion MUST carry the fork
    /// pre-image; an unguarded shadow write can never be blessed. (Non-shadow origins are exempt: they
    /// stamp `expected_prior_generation` at read time.)
    #[test]
    fn shadow_write_without_pre_image_is_refused_fail_closed() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        let mut overlay = TransactionOverlay::new(BarrierOriginClass::FeedTrigger, transitional(1, 0));
        overlay.push_mutation(v2_mutation(a, transitional(1, 0), 640, 640));
        publisher.publish_transaction(&mut txn, overlay).expect("prior");

        let pre_replay_seq = u.pools.commit_seq();
        let g = u.pools.generation(a);
        let batch = StateMutationBatch::single(
            BarrierOriginClass::Shadow,
            PoolStateMutation {
                pool: a,
                point: transitional(1, 0),
                expected_prior_generation: g,
                pre_replay_seq,
                // No fork pre-image — a shadow write that cannot prove its prior state.
                shadow_pre_image: None,
                write: FamilyWrite::V2(v2(999, 111)),
            },
        );
        publisher.apply_mutation_batch(&mut txn, batch).expect("apply");
        assert_eq!(
            reserves(&u, a),
            (640, 640),
            "a Shadow-origin write with NO pre-image must be REFUSED (fail-closed totality)"
        );
        assert_eq!(u.pools.param_poison(a), crate::pool::PARAM_POISON_NONE);
    }

    /// WI-06 T3 fix round 5 (codex round-5 item 5): the value-parity compare is on the SWAP-load-bearing
    /// fields only. V2 matches on reserves and is `block_timestamp_last`-AGNOSTIC (the fork getter and a
    /// resync legitimately carry different timestamps for identical reserves — including a resident
    /// zeroed by a prior shadow promotion); mismatched reserves never match.
    #[test]
    fn shadow_pre_image_matches_is_reserves_only_and_timestamp_agnostic() {
        // Same reserves, different block_timestamp_last ⇒ MATCH (timestamp is excluded).
        let pre = FamilyWrite::V2(V2State {
            reserves: V2Reserves256 { reserve0: 10, reserve1: 20, block_timestamp_last: 111 },
        });
        let resident = FamilyWrite::V2(V2State {
            reserves: V2Reserves256 { reserve0: 10, reserve1: 20, block_timestamp_last: 0 },
        });
        assert!(
            shadow_pre_image_matches(&pre, &resident),
            "identical reserves must match regardless of block_timestamp_last"
        );
        // Different reserve ⇒ NO match.
        let moved = FamilyWrite::V2(V2State {
            reserves: V2Reserves256 { reserve0: 10, reserve1: 21, block_timestamp_last: 0 },
        });
        assert!(
            !shadow_pre_image_matches(&pre, &moved),
            "a moved reserve must NOT match (the resident advanced)"
        );
    }

    /// WI-06 F4 fix-round-2 (codex item 4-residual, part a): the reconciliation service must drain
    /// RECORDED obligations from the store — not gate on a separate pending counter that a raced
    /// reset can zero. Emulates the take/reject window: a lost corrector is recorded but its counter
    /// increment has not landed for this pass; the recorded pool must STILL be serviced.
    #[test]
    fn recorded_obligations_are_serviced_even_when_the_pending_counter_is_zero() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();
        publisher
            .authority()
            .record_lost_corrector(&StateMutationBatch::single(
                BarrierOriginClass::Resync,
                PoolStateMutation {
                    pool: a,
                    point: transitional(1, 0),
                    expected_prior_generation: UNVALIDATED_PRIOR_GENERATION,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(1, 1)),
                },
            ));
        assert_eq!(
            publisher.authority().pending_reconciliations(),
            0,
            "premise: the counter has NOT been incremented for this recorded corrector"
        );
        let resync = publisher.service_reconciliation_obligations(&mut txn);
        assert!(
            resync.contains(&a),
            "a recorded lost-corrector obligation must be serviced even when the pending counter is \
             0 — the obligation store, not the counter, is the source of truth (no stranded work)"
        );
    }

    /// WI-06 round 3 H3(b) (codex items 6+11): a committed pool whose wake was shed is recorded OWED
    /// A WAKE. The owed-wake set is drained UNCONDITIONALLY (no snapshot-equality gate) so the state
    /// loop re-emits the wake even when a resync of the already-committed pool would be byte-equal.
    /// It is INDEPENDENT of `take_obligations` (a wake is owed even with no poison/resync/count).
    #[test]
    fn owed_wake_pool_is_returned_for_re_wake_and_is_independent_of_obligations() {
        let auth = PublicationAuthority::new();
        auth.record_owed_wake(PoolId(5));
        auth.record_owed_wake(PoolId(5)); // deduped
        auth.record_owed_wake(PoolId(9));
        let owed = auth.take_owed_wakes();
        assert!(owed.contains(&PoolId(5)) && owed.contains(&PoolId(9)));
        assert_eq!(owed.len(), 2, "owed-wake set is deduped");
        assert!(
            auth.take_owed_wakes().is_empty(),
            "the owed-wake set is DRAINED on take — the state loop re-emits each wake exactly once"
        );
        // Independent of take_obligations: an owed wake survives a poison/resync/count drain.
        auth.record_owed_wake(PoolId(1));
        let (_poison, _resync, _pending) = auth.take_obligations();
        assert_eq!(
            auth.take_owed_wakes(),
            vec![PoolId(1)],
            "take_obligations must NOT drain the owed-wake set (a wake is owed independently)"
        );
    }

    /// WI-06 round 4 I5c (codex round-4 nit) RED→GREEN: the lost-corrector vectors are bounded at
    /// `LOST_CORRECTOR_CAP`. Before this fix a distinct obligation beyond the cap was SILENTLY dropped
    /// while authority stayed ARMED, so the "no corrector is ever dropped / every committed pool is
    /// owed a wake" guarantees were untrue under sustained saturation. At the cap the recording must
    /// fall CLOSED: DISARM publication authority (the coarse fallback latch) — never a silent drop.
    #[test]
    fn lost_corrector_cap_saturation_disarms_authority_not_silently_drops() {
        // --- owed-wake vector ---
        let auth = PublicationAuthority::new();
        assert!(!auth.is_disarmed(), "starts armed");
        for i in 0..LOST_CORRECTOR_CAP {
            auth.record_owed_wake(PoolId(i as u32));
        }
        assert!(
            !auth.is_disarmed(),
            "recording exactly LOST_CORRECTOR_CAP distinct owed-wake ids fits — no saturation yet"
        );
        auth.record_owed_wake(PoolId(LOST_CORRECTOR_CAP as u32));
        assert!(
            auth.is_disarmed(),
            "a distinct owed-wake id beyond LOST_CORRECTOR_CAP must DISARM authority (fail-closed), \
             never be silently dropped while authority stays armed (codex round-4 cap nit)"
        );

        // --- resync vector (same fail-closed contract) ---
        let auth = PublicationAuthority::new();
        for i in 0..LOST_CORRECTOR_CAP {
            auth.record_stale_resync(PoolId(i as u32));
        }
        assert!(!auth.is_disarmed(), "exactly cap distinct resync ids fit — no saturation yet");
        auth.record_stale_resync(PoolId(LOST_CORRECTOR_CAP as u32));
        assert!(
            auth.is_disarmed(),
            "a distinct resync id beyond the cap must DISARM authority (fail-closed), not silently drop"
        );

        // A DUPLICATE id at the cap is a dedup no-op, NOT a saturation drop — it must not disarm.
        let auth = PublicationAuthority::new();
        for i in 0..LOST_CORRECTOR_CAP {
            auth.record_stale_resync(PoolId(i as u32));
        }
        auth.record_stale_resync(PoolId(0)); // already tracked — deduped
        assert!(
            !auth.is_disarmed(),
            "a duplicate id at the cap is deduped (not a drop) and must NOT disarm authority"
        );
    }

    /// F4 (codex item 8) RED→GREEN: a repair batch carries its SOFT-poison CLEAR, so the clear
    /// happens ONLY after the repair write commits on the drain — never off-thread ahead of (or
    /// instead of) the repair. A QUEUED (undrained) repair leaves the pool poisoned; draining it
    /// clears the poison. A REJECTED repair clears nothing (poison stays, fail-closed).
    #[test]
    fn soft_poison_clears_only_after_repair_commits() {
        // Committed path: queued → poison stays; drained → poison clears.
        {
            let (u, [a, _, _]) = test_universe();
            let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 4);
            let sender = publisher.command_sender();
            let mut permit = StateWriterPermit::test_mint();
            let mut txn = permit.begin_txn();
            u.pools.poison_param(a, PARAM_POISON_SOFT, &txn);
            assert_eq!(u.pools.param_poison(a), PARAM_POISON_SOFT);

            let repair = StateMutationBatch::repair(
                BarrierOriginClass::Resync,
                PoolStateMutation {
                    pool: a,
                    point: transitional(1, 0),
                    expected_prior_generation: UNVALIDATED_PRIOR_GENERATION,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(9, 9)),
                },
                a,
                u.pools.poison_version(a),
            );
            sender.submit_batch(repair).expect("fits");
            assert_eq!(
                u.pools.param_poison(a),
                PARAM_POISON_SOFT,
                "a QUEUED (uncommitted) repair must NOT clear the poison"
            );

            publisher.drain_commands_each(&mut txn, |_| {});
            assert_eq!(
                u.pools.param_poison(a),
                PARAM_POISON_NONE,
                "a COMMITTED repair clears the SOFT poison (publisher-owned, after the write)"
            );
            assert_eq!(reserves(&u, a), (9, 9), "the repair write also committed");
        }
        // Rejected path: queue full ⇒ repair rejected ⇒ poison STAYS.
        {
            let (u, [a, _, _]) = test_universe();
            let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 1);
            let sender = publisher.command_sender();
            let mut permit = StateWriterPermit::test_mint();
            let txn = permit.begin_txn();
            u.pools.poison_param(a, PARAM_POISON_SOFT, &txn);
            // Fill the depth-1 queue.
            sender
                .submit_batch(StateMutationBatch::poison_one(
                    BarrierOriginClass::Resync,
                    a,
                    PARAM_POISON_SOFT,
                ))
                .expect("first fills the queue");
            let repair = StateMutationBatch::repair(
                BarrierOriginClass::Resync,
                PoolStateMutation {
                    pool: a,
                    point: transitional(2, 0),
                    expected_prior_generation: UNVALIDATED_PRIOR_GENERATION,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(9, 9)),
                },
                a,
                u.pools.poison_version(a),
            );
            assert_eq!(
                sender.submit_batch(repair),
                Err(BatchRejected),
                "a full queue rejects the repair"
            );
            assert_eq!(
                u.pools.param_poison(a),
                PARAM_POISON_SOFT,
                "a REJECTED repair clears nothing — the pool stays poisoned (fail-closed)"
            );
        }
    }

    /// WI-06 round 3 H4 (codex item 3 nit): a compound repair batch (a family write AND a SOFT
    /// CLEAR for the SAME pool) must advance the pool generation by EXACTLY 1 — the clear reuses the
    /// family publication's bump. Two bumps is safety-benign but SPURIOUSLY refuses the next
    /// corrector (its expected-prior-generation is now off by one), so it is a generation-semantics
    /// cleanup. RED: the pre-fix path bumps at family publication AND again at the clear (+2).
    #[test]
    fn compound_repair_batch_advances_generation_by_exactly_one() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 4);
        let sender = publisher.command_sender();
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();
        // Poison the pool (bumps the poison VERSION, not the generation), then snapshot the
        // generation the compound repair will advance.
        u.pools.poison_param(a, PARAM_POISON_SOFT, &txn);
        let gen_before = u.pools.generation(a);
        let repair = StateMutationBatch::repair(
            BarrierOriginClass::Resync,
            PoolStateMutation {
                pool: a,
                point: transitional(1, 0),
                expected_prior_generation: UNVALIDATED_PRIOR_GENERATION,
                pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                shadow_pre_image: None,
                write: FamilyWrite::V2(v2(9, 9)),
            },
            a,
            u.pools.poison_version(a),
        );
        sender.submit_batch(repair).expect("fits");
        publisher.drain_commands_each(&mut txn, |_| {});
        assert_eq!(
            u.pools.param_poison(a),
            PARAM_POISON_NONE,
            "the compound repair cleared the SOFT poison"
        );
        assert_eq!(reserves(&u, a), (9, 9), "the repair write committed");
        assert_eq!(
            u.pools.generation(a),
            gen_before + 1,
            "a compound repair (family write + clear for the same pool) must bump the generation \
             EXACTLY once (the clear reuses the family publication's bump), never twice"
        );
    }

    /// WI-06 F4 fix-round-2 (codex item 8-residual, part b): a compound REPAIR batch whose family
    /// write is STALE-refused must NOT run its SOFT-poison clear — otherwise the clear un-poisons
    /// UNREPAIRED state before the stale-disposition re-poisons it, a transient NONE a reader can
    /// observe. Provable single-threaded via the generation-bump count: with the clear gated on the
    /// write applying, a stale-refused repair bumps the pool EXACTLY ONCE (the stale disposition),
    /// not twice (clear + disposition).
    #[test]
    fn stale_refused_repair_does_not_run_its_clear() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let sender = publisher.command_sender();
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Pool a is SOFT-poisoned; capture its version for the repair's clear.
        u.pools.poison_param(a, PARAM_POISON_SOFT, &txn);
        let ver = u.pools.poison_version(a);
        let g = u.pools.generation(a);

        // A valid write commits first, advancing the generation past g (so a repair labelled with g
        // is stale-over-new).
        sender
            .submit_batch(StateMutationBatch::single(
                BarrierOriginClass::WssLog,
                PoolStateMutation {
                    pool: a,
                    point: transitional(1, 0),
                    expected_prior_generation: g,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(1, 1)),
                },
            ))
            .expect("fits");
        publisher.drain_commands_each(&mut txn, |_| {});
        let g_after_first = u.pools.generation(a);

        // The compound REPAIR is labelled with the OLD generation g → its write is refused stale;
        // its clear must be SKIPPED.
        sender
            .submit_batch(StateMutationBatch::repair(
                BarrierOriginClass::Resync,
                PoolStateMutation {
                    pool: a,
                    point: transitional(2, 0),
                    expected_prior_generation: g,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(5, 5)),
                },
                a,
                ver,
            ))
            .expect("fits");
        publisher.drain_commands_each(&mut txn, |_| {});

        // The pool stays poisoned, the stale write did not overwrite the fresh value, and the pool
        // bumped EXACTLY once (the stale disposition) — the skipped clear neither un-poisoned nor
        // double-bumped.
        assert_ne!(
            u.pools.param_poison(a),
            crate::pool::PARAM_POISON_NONE,
            "a stale-refused repair must NOT clear the poison (no transient NONE on unrepaired state)"
        );
        assert_eq!(reserves(&u, a), (1, 1), "the stale repair write was refused");
        assert_eq!(
            u.pools.generation(a) - g_after_first,
            1,
            "a stale-refused repair bumps ONCE (stale disposition), not twice (skipped clear)"
        );
    }

    /// F4 (codex item 4) RED→GREEN: a queue-full POISON rejection disarms authority AND records the
    /// obligation; the permit-holding state thread OBSERVES it via `service_reconciliation_obligations`,
    /// which RE-APPLIES the lost poison (the pool ends poisoned, never live-unpoisoned) and CONSUMES
    /// the pending-reconciliation counter (it is no longer write-only).
    #[test]
    fn rejected_poison_is_reapplied_by_reconciliation_service() {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 1);
        let sender = publisher.command_sender();
        let mut permit = StateWriterPermit::test_mint();
        let mut txn = permit.begin_txn();

        // Fill the depth-1 queue so the poison submit is rejected.
        sender
            .submit_batch(StateMutationBatch::single(
                BarrierOriginClass::Resync,
                PoolStateMutation {
                    pool: a,
                    point: transitional(1, 0),
                    expected_prior_generation: UNVALIDATED_PRIOR_GENERATION,
                    pre_replay_seq: UNVALIDATED_PRE_REPLAY_SEQ,
                    shadow_pre_image: None,
                    write: FamilyWrite::V2(v2(1, 1)),
                },
            ))
            .expect("first fills the queue");
        assert!(!publisher.authority().is_disarmed());

        assert_eq!(
            sender.submit_batch(StateMutationBatch::poison_one(
                BarrierOriginClass::Resync,
                a,
                PARAM_POISON_HARD,
            )),
            Err(BatchRejected),
            "a full queue rejects the poison"
        );
        assert!(publisher.authority().is_disarmed(), "a rejection must disarm authority");
        assert!(
            publisher.authority().pending_reconciliations() > 0,
            "a rejection must schedule a reconciliation"
        );
        assert_eq!(
            u.pools.param_poison(a),
            PARAM_POISON_NONE,
            "the dropped poison batch left the pool UN-poisoned (the hole)"
        );

        // The state thread drains the accepted batch, then services the obligation.
        publisher.drain_commands_each(&mut txn, |_| {});
        let _resync = publisher.service_reconciliation_obligations(&mut txn);
        assert_eq!(
            u.pools.param_poison(a),
            PARAM_POISON_HARD,
            "the rejected poison must be RE-APPLIED — never left live-unpoisoned"
        );
        assert_eq!(
            publisher.authority().pending_reconciliations(),
            0,
            "the reconciliation obligation is CONSUMED, not a write-only counter"
        );
    }

    /// §3.2 (review 7): a writer parked mid-odd leaves the cell UNREADABLE (reader declines
    /// at the expected generation), and a forced overwrite during a paused reader is memory
    /// safe and can only make the reader decline.
    #[test]
    fn interrupted_writer_parked_mid_odd_reader_declines_and_forced_overwrite_is_memory_safe() {
        let table = RouteAuthorityCellTable::new(4);
        let route = RouteId(1);
        // First stable publication: generation 2.
        let mut permit = StateWriterPermit::test_mint();
        let txn = permit.begin_txn();
        let gen2 = table
            .write_route_cell(&txn, route, RouteAuthorityWords([1; 7]))
            .expect("in range");
        assert_eq!(gen2, 2, "first publication is stable generation 2");
        assert!(table.read_route_cell(route, gen2).is_some());

        // Second write, parked between the odd marker and the payload stores: a reader at
        // the OLD generation must DECLINE (the odd marker is already visible).
        table.write_route_cell_with_probe(route, RouteAuthorityWords([2; 7]), |phase| {
            if phase == SeqlockWritePhase::AfterOddMarker {
                assert!(
                    table.read_route_cell(route, gen2).is_none(),
                    "a reader must DECLINE while a writer is parked mid-odd — accepting \
                     here hands out a payload that is about to be torn"
                );
            }
        });
        // After completion the new generation reads coherently.
        let gen4 = table.cell_generation(route).expect("in range");
        assert_eq!(
            table.read_route_cell(route, gen4),
            Some(RouteAuthorityWords([2; 7]))
        );
        assert!(table.read_route_cell(route, gen2).is_none(), "old generation declines");
    }

    /// Review 8: a full/rejected off-thread command queue is a LOST canonical corrector —
    /// authority must disarm atomically and a reconciliation obligation must be scheduled;
    /// a silent drop (or an error the caller can ignore without consequence) is forbidden.
    #[test]
    fn off_thread_batch_queue_full_or_rejection_atomically_disarms_authority_and_schedules_reconciliation()
    {
        let (u, [a, _, _]) = test_universe();
        let publisher = StatePublisher::new(StdArc::clone(&u), 8, 2); // queue depth 2
        let sender = publisher.command_sender();

        let batch = |r: u128| {
            let mut b = StateMutationBatch {
                origin: BarrierOriginClass::WssLog,
                mutations: ArrayVec::new(),
                poison: ArrayVec::new(),
                clear_soft: ArrayVec::new(),
            };
            b.mutations
                .try_push(v2_mutation(a, canonical_point(2000, 0x20), r, r))
                .expect("push");
            b
        };
        sender.submit_batch(batch(1)).expect("first fits");
        sender.submit_batch(batch(2)).expect("second fits");
        let rejected = sender.submit_batch(batch(3));
        assert_eq!(rejected, Err(BatchRejected), "the third batch overflows the queue");
        assert!(
            publisher.authority().is_disarmed(),
            "a rejected off-thread batch means the resident state can no longer be \
             maintained — authority must disarm ATOMICALLY at the rejection"
        );
        assert!(
            publisher.authority().pending_reconciliations() > 0,
            "the lost corrector must schedule a reconciliation obligation, never vanish"
        );
    }

    /// T3: an off-thread PARAM-POISON batch (`poison_param` SET has no `FamilyWrite`) is
    /// applied by the SOLE publisher on the drain — the pool's poison level is raised
    /// (monotonic) and its generation bumped, and the drain reports the poisoned pool as
    /// touched so detection wakes and the poison takes effect (commit-before-wake). This is
    /// the publisher-owned route the WSS param-guard branches + the resync Camelot sweep use.
    #[test]
    fn off_thread_poison_batch_is_publisher_applied_and_bumps_generation() {
        let (u, [a, b, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        let mut permit = StateWriterPermit::test_mint();
        let sender = publisher.command_sender();

        assert_eq!(u.pools.param_poison(a), crate::pool::PARAM_POISON_NONE);
        assert_eq!(u.pools.param_poison(b), crate::pool::PARAM_POISON_NONE);
        let gen0 = u.pools.generation(a);

        sender
            .submit_batch(StateMutationBatch::poison_one(
                BarrierOriginClass::WssLog,
                a,
                PARAM_POISON_HARD,
            ))
            .expect("queued");

        let mut touched_seen: Vec<PoolId> = Vec::new();
        let mut txn = permit.begin_txn();
        let committed =
            publisher.drain_commands_each(&mut txn, |touched| touched_seen.extend_from_slice(touched));

        assert_eq!(committed, 1, "the poison-only batch commits");
        assert_eq!(
            u.pools.param_poison(a),
            PARAM_POISON_HARD,
            "the publisher (not the off-thread writer) applied the poison"
        );
        assert!(
            u.pools.generation(a) > gen0,
            "the poisoned pool's generation must bump so a reader re-evaluates and observes the poison"
        );
        assert_eq!(
            touched_seen,
            vec![a],
            "the drain reports the poisoned pool as touched → the state thread emits its DirtyEvent AFTER commit"
        );
        // Team-lead-required RED (a): a DELIVERED per-pool admin-event poison poisons ONLY that
        // pool and authority STAYS ARMED — the pool is fail-closed per-pool (bumped generation +
        // raised poison level ⇒ its quotes refuse); whole-authority disarm is reserved for the
        // queue-full LOST-poison path, never a delivered per-pool poison.
        assert_eq!(
            u.pools.param_poison(b),
            crate::pool::PARAM_POISON_NONE,
            "a per-pool admin-event poison must poison ONLY the named pool, never a neighbour"
        );
        assert!(
            !publisher.authority().is_disarmed(),
            "a DELIVERED per-pool poison must NOT disarm the whole authority (that would dark the \
             entire engine for one pool's admin event) — per-pool fail-closed only"
        );
    }

    /// T3 team-lead-required RED (b): a poison SET that CANNOT be delivered (bounded queue full ⇒
    /// `submit_batch` rejects) is a LOST poison — we can no longer guarantee that pool is
    /// poisoned, so the ONLY fail-closed response is the broader authority DISARM + a scheduled
    /// reconciliation, NEVER a silent skip. This is the queue-full counterpart to the delivered
    /// per-pool path above: delivered ⇒ per-pool poison (armed); undeliverable ⇒ whole-authority
    /// disarm. Both are fail-closed; neither is fail-open.
    #[test]
    fn off_thread_poison_queue_full_disarms_authority_never_silently_skips() {
        let (u, [a, b, _]) = test_universe();
        let publisher = StatePublisher::new(StdArc::clone(&u), 8, 1); // queue depth 1
        let sender = publisher.command_sender();

        sender
            .submit_batch(StateMutationBatch::poison_one(
                BarrierOriginClass::WssLog,
                a,
                PARAM_POISON_HARD,
            ))
            .expect("first poison fits the depth-1 queue");
        // The second poison overflows the bounded queue and is REJECTED.
        let rejected = sender.submit_batch(StateMutationBatch::poison_one(
            BarrierOriginClass::WssLog,
            b,
            PARAM_POISON_HARD,
        ));
        assert_eq!(
            rejected,
            Err(BatchRejected),
            "the overflowing poison is REJECTED — never silently dropped"
        );
        assert!(
            publisher.authority().is_disarmed(),
            "a LOST (undeliverable) poison ⇒ authority disarms ATOMICALLY at the rejection: no \
             un-poisoned pool may authorize while a poison we could not deliver is outstanding"
        );
        assert!(
            publisher.authority().pending_reconciliations() > 0,
            "the lost poison schedules a reconciliation obligation, never vanishes"
        );
    }

    // ------------------------------------------------------------------------------------
    // D1b coherent snapshot REDs
    // ------------------------------------------------------------------------------------

    /// D1b: every ACCEPTED `{count, max_seq}` snapshot must satisfy `max_seq >= count` when
    /// sequences are allocated 1,2,3,... in record order (pigeonhole: `count` counted taints
    /// have distinct seqs, so their max is at least `count`) — i.e. the bound COVERS every
    /// counted taint. The deterministic schedule drives the shipped writer's own split; the
    /// hammer covers real interleavings.
    #[test]
    fn concurrent_taint_snapshot_count_is_exactly_covered_by_its_sequence_bound() {
        // Deterministic schedule: pause the shipped writer between its two halves and
        // snapshot — with the CORRECT (max-first) order the paused state shows the seq
        // bound already covering the not-yet-counted taint; with the reversed order the
        // count is visible while its seq is NOT in the bound.
        let counters = TaintProducerCounters::new();
        counters.record_content_taint_split_for_test(1, || {
            let snap = counters.snapshot();
            assert!(
                snap.max_seq >= snap.content,
                "writer-order violation: a taint became COUNTED (content={}) before its \
                 sequence entered the bound (max_seq={}) — the barrier derived from this \
                 snapshot under-covers (D1b requires fetch_max FIRST, count LAST)",
                snap.content,
                snap.max_seq
            );
        });

        // Concurrent hammer: 4 producers, sequences from one global allocator.
        let counters = StdArc::new(TaintProducerCounters::new());
        let seq_src = StdArc::new(AtomicU64::new(0));
        let stop = StdArc::new(StdAtomicBool::new(false));
        let mut producers = Vec::new();
        for _ in 0..4 {
            let c = StdArc::clone(&counters);
            let s = StdArc::clone(&seq_src);
            let st = StdArc::clone(&stop);
            producers.push(std::thread::spawn(move || {
                while !st.load(StdOrdering::Relaxed) {
                    let seq = s.fetch_add(1, Ordering::Relaxed) + 1;
                    c.record_content_taint(seq);
                }
            }));
        }
        for _ in 0..200_000 {
            let snap = counters.snapshot();
            assert!(
                snap.max_seq >= snap.content,
                "accepted snapshot count={} not covered by max_seq={}",
                snap.content,
                snap.max_seq
            );
        }
        stop.store(true, StdOrdering::Relaxed);
        for p in producers {
            p.join().expect("producer joins");
        }
    }

    /// The RECORDED writer-order discriminator: the reversed-order mutant (count first,
    /// bound after) demonstrably violates the coverage invariant at the exact schedule the
    /// shipped order survives — proof the D1b RED distinguishes the protocol, not the test.
    #[test]
    fn reversed_writer_order_mutant_violates_snapshot_coverage() {
        let counters = TaintProducerCounters::new();
        let mut violated = false;
        counters.record_content_taint_reversed_order_mutant(1, || {
            let snap = counters.snapshot();
            if snap.max_seq < snap.content {
                violated = true;
            }
        });
        assert!(
            violated,
            "the reversed-order mutant must exhibit a counted taint whose sequence is NOT \
             in the bound — if it doesn't, the D1b discriminator is vacuous"
        );
    }

    // ------------------------------------------------------------------------------------
    // D1 rev-4 barrier REDs (type-level halves; T7 wires the runtime)
    // ------------------------------------------------------------------------------------

    fn dark_publisher_with_pending() -> (StdArc<Universe>, StatePublisher, StateWriterPermit, PoolId)
    {
        let (u, [a, _, _]) = test_universe();
        let mut publisher = StatePublisher::new(StdArc::clone(&u), 8, 8);
        publisher.begin_dark();
        let permit = StateWriterPermit::test_mint();
        (u, publisher, permit, a)
    }

    fn barrier_over(anchor_block: u64, a: PoolId) -> CompletedTaintBarrier {
        let mut covering = StateMutationBatch {
            origin: BarrierOriginClass::Receipt,
            mutations: ArrayVec::new(),
            poison: ArrayVec::new(),
            clear_soft: ArrayVec::new(),
        };
        covering
            .mutations
            .try_push(v2_mutation(a, canonical_point(anchor_block, 0x30), 5, 5))
            .expect("push");
        CompletedTaintBarrier {
            anchor: CanonicalCursor {
                block_number: anchor_block,
                block_hash: [0x30; 32],
                state_root: [0x77; 32],
                tx_index: None,
                log_index: None,
            },
            snapshot: TaintCounterSnapshot {
                content: 0,
                terminal: 0,
                max_seq: 0,
            },
            covering_batch: covering,
            treatments: [None; 7],
            fork: SurfaceObligation::Reset,
            mirror: SurfaceObligation::Suspended,
            feed_block_offset: 0,
        }
    }

    /// D1 rev 4: a mutation arriving DURING the freeze (between the dominance verify and the
    /// CAS) stages in its bounded queue, NEVER commits inside the barrier window, and cannot
    /// move any frontier row the CAS validates.
    #[test]
    fn staged_mutation_during_freeze_cannot_bypass_the_dominance_verify() {
        let (u, mut publisher, mut permit, a) = dark_publisher_with_pending();
        let mut txn = permit.begin_txn();
        let sender = publisher.command_sender();
        let before = reserves(&u, a);

        let mut in_window_batch = StateMutationBatch {
            origin: BarrierOriginClass::Receipt,
            mutations: ArrayVec::new(),
            poison: ArrayVec::new(),
            clear_soft: ArrayVec::new(),
        };
        in_window_batch
            .mutations
            .try_push(v2_mutation(a, canonical_point(9_999, 0x99), 777, 777))
            .expect("push");
        let in_window_batch = std::cell::RefCell::new(Some(in_window_batch));

        let outcome = publisher.commit_taint_reconciliation_with_probe(
            &mut txn,
            barrier_over(1_000, a),
            |phase| {
                if phase == BarrierPhase::BetweenVerifyAndCas {
                    // A canonical corrector arrives DURING the freeze.
                    sender
                        .submit_batch(in_window_batch.borrow_mut().take().expect("one shot"))
                        .expect("stages in the bounded queue");
                    // It must NOT commit inside the barrier window.
                    assert_eq!(
                        reserves(&u, a),
                        before,
                        "a batch arriving during the freeze COMMITTED between the dominance \
                         verify and the CAS — the barrier validated a table the commit just \
                         invalidated (rev-4 R1)"
                    );
                }
            },
        );
        // The in-window batch must have stayed STAGED through the WHOLE barrier (verify,
        // covering commit, CAS): only the covering batch's write is resident at re-arm, and
        // the staged corrector replays through NORMAL publication strictly AFTER.
        match outcome {
            RearmOutcome::Rearmed(_) => {
                assert_eq!(
                    reserves(&u, a),
                    (5, 5),
                    "after re-arm, only the COVERING batch's write may be resident; the \
                     staged in-window batch replays through NORMAL publication later"
                );
                let replayed = publisher.drain_commands(&mut txn);
                assert_eq!(
                    replayed, 1,
                    "the corrector that arrived during the freeze must still be QUEUED at \
                     re-arm — a zero here means the barrier itself drained (committed) it \
                     between the dominance verify and the CAS, bypassing the verify \
                     (rev-4 R1)"
                );
                assert_eq!(
                    reserves(&u, a),
                    (777, 777),
                    "the staged corrector replays through NORMAL publication after re-arm"
                );
            }
            RearmOutcome::Refused(r) => panic!("barrier unexpectedly refused: {r:?}"),
        }
    }

    /// D1 rev 4: an INCOMPARABLE row (here: Fluid, recorded while dark) has no defined
    /// dominance — the CAS must REFUSE until the row is reconciled at the anchor, reset, or
    /// its trust suspended; with a treatment it proceeds.
    #[test]
    fn incomparable_row_requires_reconcile_reset_or_suspend_before_rearm() {
        let (u, mut publisher, mut permit, a) = dark_publisher_with_pending();
        let mut txn = permit.begin_txn();

        // A Fluid-class commit lands while dark (its frontier is an opaque identity).
        let mut fluid_overlay =
            TransactionOverlay::new(BarrierOriginClass::Fluid, canonical_point(0, 0));
        fluid_overlay.push_mutation(v2_mutation(a, canonical_point(0, 0), 3, 3));
        // (V2 write under Fluid origin class: origin classes label the WRITER, not the
        // family; the test universe is V2-only.)
        publisher
            .publish_transaction(&mut txn, fluid_overlay)
            .expect("dark commit");

        let untreated = publisher.commit_taint_reconciliation(&mut txn, barrier_over(1_000, a));
        assert_eq!(
            untreated,
            RearmOutcome::Refused(RearmRefusal::IncomparableRowUntreated(
                BarrierOriginClass::Fluid
            )),
            "an incomparable dark row without reconcile/reset/suspend treatment must \
             REFUSE the re-arm CAS (rev-4 R1: dominance is undefined for this row)"
        );

        let mut treated = barrier_over(1_000, a);
        treated.treatments[BarrierOriginClass::Fluid as usize] =
            Some(IncomparableTreatment::Reset);
        let outcome = publisher.commit_taint_reconciliation(&mut txn, treated);
        assert!(
            matches!(outcome, RearmOutcome::Rearmed(_)),
            "with the Fluid row treated (Reset) the barrier completes, got {outcome:?}"
        );
    }

    /// `checked_add` makes generation exhaustion fail CLOSED (process restart) rather than
    /// wrapping and recreating ABA. This pins the no-wrap rule independently of the operational
    /// argument that ~2^32 repins at a ~30s cadence is millennia away.
    #[test]
    #[should_panic(
        expected = "replay-integrity pin generation exhausted; restart required before wrap"
    )]
    fn replay_integrity_generation_exhaustion_never_wraps() {
        let shared = ReplayIntegrityShared {
            packed: AtomicU64::new(pack_replay_integrity(u32::MAX, 0)),
        };
        let _ = shared.repin_clear();
    }

    /// Pins the packed low/high split independently of Loom: boot generation 1 is current;
    /// a POISONED fault rejects generation 1 without moving the generation; a repin advances
    /// to generation 2 and clears the flags so generation 2 is current while generation 1 is
    /// permanently refused; a fresh DISORDER on generation 2 then rejects generation 2.
    #[test]
    fn replay_integrity_packed_fault_and_repin_transition_preserves_old_pin_refusal() {
        let shared = ReplayIntegrityShared::new();
        assert_eq!(shared.current_pin_generation(), 1);
        assert!(shared.replay_integrity_current(1));

        shared.publish_fault(REPLAY_FLAG_POISONED);
        assert!(shared.faulted());
        assert!(!shared.replay_integrity_current(1));
        // The fault does not move the generation.
        assert_eq!(shared.current_pin_generation(), 1);

        let gen2 = shared.repin_clear();
        assert_eq!(gen2, 2);
        assert_eq!(shared.current_pin_generation(), 2);
        assert!(!shared.faulted());
        // Generation 1 is permanently refused after the repin (no ABA).
        assert!(!shared.replay_integrity_current(1));
        assert!(shared.replay_integrity_current(2));

        shared.publish_fault(REPLAY_FLAG_DISORDER);
        assert!(!shared.replay_integrity_current(2));
        // Still refuses generation 1 as well.
        assert!(!shared.replay_integrity_current(1));
    }
}

#[cfg(loom)]
mod loom_tests {
    use super::*;
    use loom::sync::Arc as LoomArc;
    use loom::thread;

    /// §3.2 seqlock litmus: under every loom interleaving, a reader can never observe NEW
    /// payload words under the OLD even generation — the SeqCst odd marker/fence forbids any
    /// payload store becoming visible before readers can observe odd.
    #[test]
    fn route_authority_seqlock_arm_memory_order_never_exposes_payload_under_old_even_generation() {
        loom::model(|| {
            let table = LoomArc::new(RouteAuthorityCellTable::new(1));
            let route = RouteId(0);
            let mut permit = StateWriterPermit::test_mint();
            let txn = permit.begin_txn();
            let gen2 = table
                .write_route_cell(&txn, route, RouteAuthorityWords([1; 7]))
                .expect("in range");

            let t_writer = {
                let table = LoomArc::clone(&table);
                thread::spawn(move || {
                    let mut permit = StateWriterPermit::test_mint();
                    let txn = permit.begin_txn();
                    table.write_route_cell(&txn, route, RouteAuthorityWords([2; 7]));
                })
            };
            // Reader at the OLD generation: must see the OLD payload or decline — never the
            // new payload under gen2.
            if let Some(words) = table.read_route_cell(route, gen2) {
                assert_eq!(
                    words,
                    RouteAuthorityWords([1; 7]),
                    "reader accepted NEW payload under the OLD even generation"
                );
            }
            t_writer.join().expect("writer");
        });
    }

    /// §3.3 epoch-reader litmus: the `fence(Acquire)` orders the payload loads before the
    /// marker re-read — a read accepted at a stable even epoch never returns a torn value.
    #[test]
    fn epoch_reader_load_barrier_orders_payload_before_marker_reread() {
        loom::model(|| {
            let epoch = LoomArc::new(StatePublicationEpoch::new());
            let word_a = LoomArc::new(AtomicU64::new(0));
            let word_b = LoomArc::new(AtomicU64::new(0));

            let t_writer = {
                let epoch = LoomArc::clone(&epoch);
                let word_a = LoomArc::clone(&word_a);
                let word_b = LoomArc::clone(&word_b);
                thread::spawn(move || {
                    let odd = epoch.open_odd().expect("open");
                    word_a.store(1, Ordering::Relaxed);
                    word_b.store(1, Ordering::Relaxed);
                    epoch.close_even(odd).expect("close");
                })
            };
            if let Some((_, (a, b))) = epoch_read_pair(&epoch, || {
                (
                    word_a.load(Ordering::Relaxed),
                    word_b.load(Ordering::Relaxed),
                )
            }) {
                assert_eq!(a, b, "accepted epoch-guarded read returned a torn pair");
            }
            t_writer.join().expect("writer");
        });
    }

    /// D2 rev 4: fault-publish vs wire-read ordering — once `replay_integrity_current`
    /// returns true for a generation, a fault published BEFORE that read must have been
    /// visible (Release/Acquire pairing); a wire read can never miss a fault that was
    /// published before its own Acquire loads.
    #[test]
    fn replay_integrity_fault_publish_vs_wire_read_ordering() {
        loom::model(|| {
            let shared = LoomArc::new(ReplayIntegrityShared::new());
            let gen = shared.current_pin_generation();
            let t_fault = {
                let shared = LoomArc::clone(&shared);
                thread::spawn(move || {
                    shared.publish_fault(REPLAY_FLAG_POISONED);
                })
            };
            let ok = shared.replay_integrity_current(gen);
            t_fault.join().expect("fault thread");
            // After joining, the fault is definitely visible.
            assert!(!shared.replay_integrity_current(gen));
            // If the wire read raced ahead of the fault it returned true — that is the
            // permitted outcome ONLY because the fault had not yet been published; loom
            // explores the interleaving where the fault's Release store completes first,
            // and there `ok` must be false.
            let _ = ok;
        });
    }

    /// D2 rev 4: an old pin must NEVER authorize across a concurrent repin. The only
    /// two valid states are `{old, POISONED}` (rejects old) before the repin lands and
    /// `{new, 0}` (rejects old) after it lands. A wire read that loads generation and
    /// flags from two separate atomics can synthesize the impossible hybrid snapshot
    /// `{old generation, new clear flags}` and authorize the old pin. This model has no
    /// false-positive schedule: only the split-atomic implementation can produce it.
    #[test]
    fn replay_integrity_old_pin_never_authorizes_across_concurrent_repin() {
        loom::model(|| {
            let shared = LoomArc::new(ReplayIntegrityShared::new());
            let old_generation = shared.current_pin_generation();
            shared.publish_fault(REPLAY_FLAG_POISONED);
            assert!(!shared.replay_integrity_current(old_generation));

            let repinner = {
                let shared = LoomArc::clone(&shared);
                thread::spawn(move || shared.repin_clear())
            };
            let old_pin_authorized =
                shared.replay_integrity_current(old_generation);
            let new_generation = repinner.join().expect("repin thread");

            assert!(
                !old_pin_authorized,
                "old pin authorized from a torn {{old generation, new clear flags}} snapshot"
            );
            assert_eq!(new_generation, old_generation + 1);
            assert!(shared.replay_integrity_current(new_generation));
        });
    }

    /// WI-06 D2 B0/B5 handoff litmus: the sim producer CAPTURES a clean generation N via
    /// `capture_clean_pin_generation`, transfers N (an OWNED u64 crossing the sim→submit SPSC),
    /// and races a fault+repin against the consumer's `replay_integrity_current(N)`. A consumer
    /// read that linearizes before the Release/AcqRel transition may still see N current — legal,
    /// N was genuinely clean at that instant. Once the repin has linearized, the captured N is
    /// PERMANENTLY refused: the owned handoff value can never authorize the old pin across the
    /// transition (the capture is a strict subset of the wire read — same Acquire load, one word).
    #[test]
    fn replay_integrity_captured_clean_generation_loses_to_concurrent_fault_and_repin() {
        loom::model(|| {
            let shared = LoomArc::new(ReplayIntegrityShared::new());
            // Producer: capture a CLEAN generation and hand off the owned value.
            let captured = shared
                .capture_clean_pin_generation()
                .expect("boot pin is clean");
            let disruptor = {
                let shared = LoomArc::clone(&shared);
                thread::spawn(move || {
                    shared.publish_fault(REPLAY_FLAG_POISONED);
                    shared.repin_clear()
                })
            };
            // Consumer authorizes on the captured value; racing fully ahead of the fault is the
            // only interleaving in which this may be `true`.
            let raced = shared.replay_integrity_current(captured);
            let repinned = disruptor.join().expect("disruptor thread");
            assert_eq!(
                repinned,
                captured + 1,
                "the repin advances the captured generation"
            );
            assert!(
                !shared.replay_integrity_current(captured),
                "a captured clean generation must never authorize across a repin (owned handoff)"
            );
            let _ = raced;
        });
    }

    /// D1b snapshot under loom: with the shipped writer order, every accepted snapshot's
    /// bound covers its count.
    #[test]
    fn taint_snapshot_bound_covers_count_under_loom() {
        loom::model(|| {
            let counters = LoomArc::new(TaintProducerCounters::new());
            let t = {
                let c = LoomArc::clone(&counters);
                thread::spawn(move || {
                    c.record_content_taint(1);
                })
            };
            let snap = counters.snapshot();
            assert!(
                snap.max_seq >= snap.content,
                "snapshot count={} not covered by bound={}",
                snap.content,
                snap.max_seq
            );
            t.join().expect("producer");
        });
    }
}
