//! Stage-1 "Mirror v2" journal + reconcile primitives — the **Gate-C net** the sampled shadow
//! mirror lacked (that omission is why the earlier `REVM_LOCAL_STATE` mirror drifted ~79% and was
//! shelved). These are *pure*, singleton-free, unit-testable building blocks; the live wiring
//! (feed-advancer thread, block-boundary reconcile task, metrics) lives in
//! [`crate::shadow_revm`] / [`crate::pipeline`].
//!
//! The four Gate-C guarantees this module implements against a [`StateMirror`]:
//!
//! 1. **Journal + idempotency.** Every applied feed tx is recorded by `(block, tx_hash)` BEFORE its
//!    slots hit the mirror (record-then-apply, Fulcrum `state_mirror/storage.rs:254-261` semantics).
//!    A duplicate `(block, tx_hash)` is a no-op; a tx for an already-closed (older) block is
//!    rejected. Mantis's [`ChangedSlot`] already carries `original` AND `present`, so a journal
//!    entry simply *is* the changed-slot vec (Fulcrum needed a separate diff record).
//! 2. **Exact-inverse rollback.** Undo a block by writing each entry's `original` back in REVERSE
//!    order (Fulcrum `storage.rs:469-501`, ~40 lines here against [`StateMirror::apply_batch`]).
//!    Because the advancer feeds the fork AND the mirror the identical ordered stream from the same
//!    pin, `ChangedSlot.original` equals the mirror's pre-tx value for that slot, so reverse-order
//!    replacement restores the block's start state value-exactly.
//! 3. **Canonical reconcile + repair.** Per closed block, compare a bounded sample of the block's
//!    journal footprint against a canonical [`StateFetcher`]; on mismatch, roll the footprint back,
//!    re-fetch canonical, re-apply, and re-tag (Fulcrum `validation/reconciliation.rs:624-670` +
//!    repair-footprint `storage.rs:505-545`, done per-block instead of per-receipt). Selection is
//!    [`ReconcileMode`]: `Logs` (full footprint), `Sampled` (deterministic hash subset, Fulcrum
//!    `sampling.rs:54-60`), or `Off`.
//! 4. **Degraded-mode honesty.** A block that suffered a lossless-queue gap / aborted apply is
//!    flagged; a flagged block is ALWAYS fully reconciled regardless of mode — never silently
//!    skipped.
//!
//! In-RAM by default (last N blocks). [Stage 5] OPTIONAL disk persistence via
//! [`crate::mirror_journal_persist`] (redb, not the unmaintained sled Fulcrum flags in its own
//! SEC-046 note) resumes a warm journal across a restart; default off ⇒ in-RAM-only, byte-identical.

use std::collections::VecDeque;

use ahash::{AHashMap, AHashSet};
use alloy_primitives::U256;
use engine_revm::{CanonicalFetchErrorKind, ChangedSlot, Proven, StateFetcher};

use crate::state_mirror::StateMirror;

/// [WP1 · Fix B3] Cap on carried reconciliation debt slots before escalating to a full resync. When
/// exceeded the mirror stays untrusted and the operation demands an explicit full resync — debt is
/// NEVER silently evicted.
const MAX_DEBT_SLOTS: usize = 8192;
/// [WP1 · Fix B3] Max age (in closed blocks) carried debt may persist before full-resync escalation.
const MAX_DEBT_AGE_BLOCKS: u64 = 128;

/// One journaled feed-tx application. The changed-slot vec IS the entry (each [`ChangedSlot`]
/// carries the pre-value `original` and post-value `present`), so rollback needs no separate record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalEntry {
    /// keccak256 of the signed-tx envelope — the idempotency key together with the block.
    pub tx_hash: [u8; 32],
    /// The tx's full changed-slot set (uncapped), in execution order.
    pub slots: Vec<ChangedSlot>,
}

/// All journaled entries for one block, plus the degraded-mode reconcile flag.
struct BlockJournal {
    block: u64,
    entries: Vec<JournalEntry>,
    /// Set when this block suffered a gap/abort (feed-queue overflow, replay error mid-block); a
    /// flagged block is force-reconciled at close regardless of [`ReconcileMode`].
    reconcile_flagged: bool,
}

/// Outcome of [`MirrorJournal::record_and_apply`].
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ApplyResult {
    /// New `(block, tx_hash)` — recorded and applied to the mirror.
    Applied,
    /// `(block, tx_hash)` already journaled — no-op (idempotent).
    DuplicateSkipped,
    /// Block is older than the journal head — the ordered stream regressed; rejected (never
    /// silently applied out of order).
    OutOfOrderRejected,
}

/// [B5] Outcome of [`MirrorJournal::rollback_to`] — a journal-driven multi-block unwind triggered
/// when the feed regresses DEEPER than one block (a reorg). The one-block per-block reconcile path
/// stays as-is; this is the active repair for a genuine multi-block rewind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackTo {
    /// Every journaled block strictly newer than `target` was exact-inverse rolled back
    /// (newest-block-first, so a slot touched across the range composes back to its end-of-`target`
    /// value), pruned from the ring, and its `(block, tx_hash)` idempotency keys purged; the mirror
    /// now holds the value-exact END-OF-`target` state. Carries the deduped union footprint of the
    /// unwound range (the reconcile candidate set — see [`reconcile_footprint`]) and how many blocks
    /// were undone.
    Unwound {
        /// Deduped `(address, slot)` union across every unwound block.
        footprint: Vec<([u8; 20], U256)>,
        /// Number of blocks exact-inverse rolled back.
        blocks_unwound: u64,
    },
    /// `target` is below the OLDEST retained block — the ring no longer holds the inverses needed to
    /// exact-unwind that deep. NO mutation was performed; the caller degrades (mark unhealthy) and
    /// full-resyncs (the existing degrade path). Counted as `reorg_beyond_journal`.
    BeyondJournal,
    /// `target` is at/after the journal head — nothing newer to unwind (not a regression). No-op.
    NoOp,
}

/// Which of a block's footprint slots the reconcile deep-compares (env `MIRROR_RECONCILE`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ReconcileMode {
    /// Compare the whole (bounded) footprint each block — canonical-log-precise resident set is a
    /// subset of the mirror's own footprint, so this is a strict superset check.
    Logs,
    /// Deterministic hash-selected subset for a low-rate deep compare (Fulcrum `sampling.rs`).
    Sampled,
    /// No reconcile (journal + rollback only).
    Off,
}

impl ReconcileMode {
    /// Parse `MIRROR_RECONCILE` (default [`ReconcileMode::Logs`] — the full Gate-C net).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "none" => ReconcileMode::Off,
            "sampled" | "sample" => ReconcileMode::Sampled,
            _ => ReconcileMode::Logs,
        }
    }
}

/// What a [`MirrorJournal::reconcile_block`] pass did — surfaced as `mirror_reconcile_*` metrics.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct ReconcileReport {
    /// Footprint slots deep-compared against canonical this pass.
    pub checked: usize,
    /// Slots whose mirror value diverged from canonical.
    pub mismatched: usize,
    /// Footprint slots rewritten from canonical during repair (0 when clean).
    pub repaired: usize,
    /// This block was force-reconciled because it was degraded-flagged.
    pub forced: bool,
}

/// [WP1 · Fix B3] Carried reconciliation debt: the `(address, slot)` footprint whose canonical
/// consistency could not be concluded (a proof/anchor/fetch failure), to be re-proved at a newer
/// CURRENT closed block before mirror trust can recover. A block flag alone is not a retry queue — an
/// unrelated clean sample must never clear this; only a complete re-proof (or full resync) does.
#[derive(Debug, Default, Clone)]
pub struct ReconcileDebt {
    slots: AHashSet<([u8; 20], U256)>,
    /// The block at which the debt was first opened (age basis for full-resync escalation).
    since_block: u64,
    /// How many times reconcile has attempted (and failed) to discharge this debt.
    attempts: u64,
    /// The most recent typed reason a discharge attempt failed (telemetry).
    last_error: Option<CanonicalFetchErrorKind>,
}

impl ReconcileDebt {
    /// Whether any debt is outstanding.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
    /// Number of outstanding debt slots.
    pub fn len(&self) -> usize {
        self.slots.len()
    }
    /// Outstanding debt slots (order unspecified).
    pub fn slots(&self) -> Vec<([u8; 20], U256)> {
        self.slots.iter().copied().collect()
    }
    /// The block the current debt was first opened at (0 when empty).
    pub fn since_block(&self) -> u64 {
        self.since_block
    }
    /// Failed discharge attempts against the current debt.
    pub fn attempts(&self) -> u64 {
        self.attempts
    }
    /// The most recent failure reason (telemetry).
    pub fn last_error(&self) -> Option<CanonicalFetchErrorKind> {
        self.last_error
    }

    /// Union `slots` into the debt (opening `since_block` if newly non-empty), bump the attempt
    /// counter, and record the latest failure reason.
    fn add(
        &mut self,
        slots: impl IntoIterator<Item = ([u8; 20], U256)>,
        block: u64,
        err: Option<CanonicalFetchErrorKind>,
    ) {
        if self.slots.is_empty() {
            self.since_block = block;
        }
        for kv in slots {
            self.slots.insert(kv);
        }
        self.attempts = self.attempts.saturating_add(1);
        if err.is_some() {
            self.last_error = err;
        }
    }

    /// Discharge all debt (only after a complete re-proof commits, or on full resync).
    fn clear(&mut self) {
        self.slots.clear();
        self.since_block = 0;
        self.attempts = 0;
        self.last_error = None;
    }
}

/// [WP1 · Fix B1] The rich result of a checked reconcile pass. Unlike [`ReconcileReport`] these states
/// do NOT collapse: successfully checked, checked+mismatched, unavailable/unverified (a canonical
/// failure — NOT a mismatch and NOT a successful check), fully repaired, incomplete-with-no-mutation,
/// and publishable/certified are all distinct. The trust decision reads this, not counters alone.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// Footprint/debt slots successfully staged from canonical and compared this pass.
    pub checked: usize,
    /// Staged slots whose mirror value diverged from canonical (only among successful fetches).
    pub mismatched: usize,
    /// Footprint slots written from canonical during the all-or-none repair commit (0 when clean).
    pub repaired: usize,
    /// This block was force-reconciled because it was degraded-flagged.
    pub forced: bool,
    /// Canonical values that could not be resolved (proof/anchor/fetch failure). A positive count
    /// means the pass entered the checked boundary and FAILED — never counted as a mismatch.
    pub canonical_failures: usize,
    /// The pass concluded with zero canonical failures (every required value was resolved). An
    /// incomplete pass performs no mutation.
    pub complete: bool,
    /// Safe to publish this block's snapshot: complete, no outstanding debt, no full-resync demand.
    pub publishable: bool,
    /// The outstanding debt footprint after this pass (empty when fully reconciled).
    pub unresolved: Vec<([u8; 20], U256)>,
    /// Debt exceeded its size/age cap: the mirror must be fully resynced, never silently evicted.
    pub full_resync_required: bool,
    /// The most recent typed canonical-failure reason (telemetry).
    pub last_error: Option<CanonicalFetchErrorKind>,
}

/// In-RAM ring of the last `cap_blocks` blocks of journaled feed-tx applications, with
/// `(block, tx_hash)` idempotency, exact-inverse rollback, and canonical reconcile+repair.
///
/// NOT thread-safe on its own — the advancer owns it single-threaded on core 0; the [`StateMirror`]
/// it writes is the shared, lock-guarded object the (Stage-2) validator would read.
pub struct MirrorJournal {
    blocks: VecDeque<BlockJournal>,
    seen: AHashSet<(u64, [u8; 32])>,
    head_block: u64,
    cap_blocks: usize,
    /// Blocks evicted from the ring while still un-reconciled (a coverage gap in the honesty
    /// ledger) — surfaced so the operator sees the ring is too small for the burst.
    evicted_unreconciled: u64,
    /// [B5] Multi-block reorg-unwind honesty ledgers (single-threaded advancer owns the journal, so
    /// plain `u64`, no atomics). `reorg_unwinds` = successful multi-block unwind operations;
    /// `reorg_blocks_unwound` = total blocks exact-inverse rolled back across them;
    /// `reorg_beyond_journal` = regressions too deep for the ring (degraded to unhealthy + resync).
    reorg_unwinds: u64,
    reorg_blocks_unwound: u64,
    reorg_beyond_journal: u64,
    /// [Stage 5] Optional disk backing (`MIRROR_JOURNAL_PERSIST`). `None` ⇒ in-RAM-only,
    /// byte-identical to Stage 1: no file is opened and no persistence call fires.
    persist: Option<crate::mirror_journal_persist::JournalPersist>,
    /// [WP1 · Fix B3] Carried reconciliation debt (proof/anchor/fetch failures awaiting re-proof at a
    /// current closed block). Empty in the healthy steady state; only a complete re-proof or full
    /// resync clears it — an unrelated clean block cannot.
    debt: ReconcileDebt,
    /// [WP1 · FIX-1] STICKY full-resync latch. Set when carried debt exceeds its size/age cap: the
    /// escalated slots were NEVER proven, so the mirror must stay untrusted until an explicit, complete
    /// re-hydration (or operator full resync) clears it. A plain clean close CANNOT clear it (that is
    /// the debt-laundering bug this fixes) — `reset()` deliberately does NOT touch it, and the advancer
    /// health verdict + publish gate consult it as a dominating input. In WP1 it is cleared ONLY by
    /// [`clear_full_resync_pending`](Self::clear_full_resync_pending), which no in-process reconcile
    /// path calls — so once escalated it is effectively latched for the run until resync tooling /
    /// restart runs. WP2 formalizes it as the `FULL_RESYNC_REQUIRED` reason bit.
    full_resync_pending: bool,
}

impl MirrorJournal {
    /// Empty journal keeping at most `cap_blocks` blocks (`MIRROR_JOURNAL_BLOCKS`, default 64).
    /// In-RAM only (no persistence) — see [`with_persistence`](Self::with_persistence).
    pub fn new(cap_blocks: usize) -> Self {
        Self {
            blocks: VecDeque::new(),
            seen: AHashSet::new(),
            head_block: 0,
            cap_blocks: cap_blocks.max(1),
            evicted_unreconciled: 0,
            reorg_unwinds: 0,
            reorg_blocks_unwound: 0,
            reorg_beyond_journal: 0,
            persist: None,
            debt: ReconcileDebt::default(),
            full_resync_pending: false,
        }
    }

    /// [Stage 5 · WP5 · F3] Attach disk backing WITHOUT installing anything. Load is not activation:
    /// the fresh journal starts EMPTY (no `seen` keys, `head_block == 0`, no ring), so a persisted
    /// `(block, tx_hash)` can NOT return [`ApplyResult::DuplicateSkipped`] and a persisted entry can NOT
    /// drive `rollback_to` until the canonical-recovery protocol
    /// ([`crate::mirror_journal_persist::JournalPersist::load_candidate`] →
    /// `recover_and_validate` → [`activate`](Self::activate)) proves the records canonical and hydrates a
    /// fresh mirror at one checked anchor. Disk bytes have zero effect on trust until then.
    pub fn with_persistence(
        cap_blocks: usize,
        persist: crate::mirror_journal_persist::JournalPersist,
    ) -> Self {
        let mut j = Self::new(cap_blocks);
        j.persist = Some(persist);
        j
    }

    /// [Stage 5 · WP5 · F1] Persist a closed block's journaled entries + provenance + journal status in
    /// ONE crash-atomic redb transaction (no-op when persistence is off). Called at block close, AFTER
    /// reconcile — one bounded write per block. A block is persisted for warm-resume ONLY when its
    /// canonical identity (`prov`) is known (the checked path resolves it); `None` ⇒ skip (an
    /// unprovenanced record could never be trusted on resume, so storing it is pointless).
    pub fn persist_close(
        &self,
        block: u64,
        prov: Option<&crate::mirror_journal_persist::BlockProvenance>,
    ) {
        let (Some(p), Some(prov)) = (&self.persist, prov) else {
            return;
        };
        if let Some(bj) = self.block_ref(block) {
            let status = self.journal_status();
            if let Err(e) = p.put_block(block, &bj.entries, prov, &status) {
                tracing::debug!(block, err = %e, "mirror-journal persist_close failed (degrading to RAM-only)");
            }
        }
    }

    /// [WP5 · F1] The journal-wide status to persist alongside a block: head + the sticky untrusted
    /// latches. If either latch is set at shutdown, a later load discards the whole candidate (a mirror
    /// that was untrusted cannot be warm-resumed).
    fn journal_status(&self) -> crate::mirror_journal_persist::JournalStatus {
        crate::mirror_journal_persist::JournalStatus {
            head: self.head_block,
            full_resync_pending: self.full_resync_pending,
            debt_present: !self.debt.is_empty(),
        }
    }

    /// [Stage 5] Whether disk persistence is active (test/observability hook).
    pub fn is_persistent(&self) -> bool {
        self.persist.is_some()
    }

    /// The newest block the journal has seen (0 before the first apply).
    pub fn head_block(&self) -> u64 {
        self.head_block
    }

    /// Total journaled entries currently retained across all resident blocks.
    pub fn entry_count(&self) -> usize {
        self.blocks.iter().map(|b| b.entries.len()).sum()
    }

    /// Number of blocks currently resident in the ring.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Blocks dropped from the ring before they were reconciled (honesty-ledger coverage gap).
    pub fn evicted_unreconciled(&self) -> u64 {
        self.evicted_unreconciled
    }

    /// [B5] Successful multi-block reorg-unwind operations ([`rollback_to`](Self::rollback_to)).
    pub fn reorg_unwinds(&self) -> u64 {
        self.reorg_unwinds
    }

    /// [B5] Total blocks exact-inverse rolled back across all reorg unwinds.
    pub fn reorg_blocks_unwound(&self) -> u64 {
        self.reorg_blocks_unwound
    }

    /// [B5] Regressions too deep for the ring to exact-unwind (degraded to unhealthy + full resync).
    pub fn reorg_beyond_journal(&self) -> u64 {
        self.reorg_beyond_journal
    }

    /// The oldest block currently resident in the ring (0 when empty). Below this a
    /// [`rollback_to`](Self::rollback_to) cannot exact-unwind.
    pub fn oldest_block(&self) -> u64 {
        self.blocks.front().map(|b| b.block).unwrap_or(0)
    }

    /// Whether `(block, tx_hash)` is already journaled (idempotency probe).
    pub fn contains(&self, block: u64, tx_hash: &[u8; 32]) -> bool {
        self.seen.contains(&(block, *tx_hash))
    }

    fn block_mut(&mut self, block: u64) -> Option<&mut BlockJournal> {
        self.blocks.iter_mut().find(|b| b.block == block)
    }

    fn block_ref(&self, block: u64) -> Option<&BlockJournal> {
        self.blocks.iter().find(|b| b.block == block)
    }

    /// Record-then-apply one feed tx (Gate-C guarantee 1). Rejects duplicates and out-of-order
    /// (older-than-head) blocks; otherwise journals the entry and applies its `present` values to
    /// `mirror` under a single write-lock. Advancing to a new block evicts the oldest once the ring
    /// is full.
    pub fn record_and_apply(
        &mut self,
        mirror: &StateMirror,
        block: u64,
        tx_hash: [u8; 32],
        slots: &[ChangedSlot],
    ) -> ApplyResult {
        // Idempotency: the same signed tx re-observed (feed dup / reconnect replay) is a no-op.
        if self.seen.contains(&(block, tx_hash)) {
            return ApplyResult::DuplicateSkipped;
        }
        // Ordering: the sequencer feed is monotonic; a block strictly older than head means the
        // ordered stream regressed — reject rather than corrupt the composition. (== head and
        // > head both advance/extend.)
        if block < self.head_block {
            return ApplyResult::OutOfOrderRejected;
        }
        if block > self.head_block {
            self.open_block(block);
            self.head_block = block;
        } else if self.block_ref(block).is_none() {
            // [FIX-AUDIT-1(c)] block == head_block (block < head_block was rejected above) but the ring
            // has no journal for it — e.g. after a warm-resume `activate` set head_block with an EMPTY
            // ring. OPEN it so this tx is JOURNALED (and therefore AUDITED at close) instead of being
            // silently applied-to-the-mirror-without-record (the silent-drop root cause of the
            // resume-anchor fail-open).
            self.open_block(block);
        }
        // Record BEFORE apply so a crash mid-apply still leaves a rollback-able record.
        self.seen.insert((block, tx_hash));
        if let Some(bj) = self.block_mut(block) {
            bj.entries.push(JournalEntry {
                tx_hash,
                slots: slots.to_vec(),
            });
        }
        // Apply this tx's post-values to the mirror (single lock acquisition — no torn tx visible).
        mirror.apply_batch(slots.iter().map(|s| (s.address, s.slot, s.present)));
        ApplyResult::Applied
    }

    /// Push a fresh block, evicting the oldest (and purging its idempotency keys) when full.
    fn open_block(&mut self, block: u64) {
        while self.blocks.len() >= self.cap_blocks {
            if let Some(old) = self.blocks.pop_front() {
                if old.reconcile_flagged {
                    self.evicted_unreconciled += 1;
                }
                for e in &old.entries {
                    self.seen.remove(&(old.block, e.tx_hash));
                }
                // [Stage 5] Keep disk bounded to the ring: drop the evicted block from persistence.
                if let Some(p) = &self.persist {
                    let _ = p.delete_block(old.block);
                }
            } else {
                break;
            }
        }
        self.blocks.push_back(BlockJournal {
            block,
            entries: Vec::new(),
            reconcile_flagged: false,
        });
    }

    /// Mark a block for forced reconcile (degraded-mode honesty, Gate-C guarantee 4). Called when a
    /// lossless-queue gap or a mid-block abort means the mirror may have silently skipped a tx.
    pub fn flag_reconcile(&mut self, block: u64) {
        if let Some(bj) = self.block_mut(block) {
            bj.reconcile_flagged = true;
        }
    }

    /// Whether `block` is degraded-flagged (test/observability hook).
    pub fn is_flagged(&self, block: u64) -> bool {
        self.block_ref(block).map(|b| b.reconcile_flagged).unwrap_or(false)
    }

    /// [Stage 5] The tx-hashes journaled for `block`, in application order — the candidate set the
    /// receipt-sampler (`MIRROR_RECONCILE=sampled`) draws its low-rate deep-compare sample from.
    pub fn block_tx_hashes(&self, block: u64) -> Vec<[u8; 32]> {
        self.block_ref(block)
            .map(|b| b.entries.iter().map(|e| e.tx_hash).collect())
            .unwrap_or_default()
    }

    /// The dedup union of `(address, slot)` a block's journaled txs touched — the repair candidate
    /// set (Fulcrum repair-footprint `storage.rs:505-545`).
    pub fn footprint(&self, block: u64) -> Vec<([u8; 20], U256)> {
        let mut seen: AHashSet<([u8; 20], U256)> = AHashSet::new();
        let mut out = Vec::new();
        if let Some(bj) = self.block_ref(block) {
            for e in &bj.entries {
                for s in &e.slots {
                    if seen.insert((s.address, s.slot)) {
                        out.push((s.address, s.slot));
                    }
                }
            }
        }
        out
    }

    /// Exact-inverse rollback of a block (Gate-C guarantee 2): write every entry's `original` back
    /// to `mirror`, entries and slots in REVERSE order, so the earliest `original` (the block's
    /// start-of-block value) wins for any slot touched more than once. Returns the footprint (for a
    /// subsequent canonical re-fetch). The journal record is retained (repair re-applies over it).
    pub fn rollback_block(&mut self, mirror: &StateMirror, block: u64) -> Vec<([u8; 20], U256)> {
        let mut restores: Vec<([u8; 20], U256, U256)> = Vec::new();
        if let Some(bj) = self.block_ref(block) {
            for e in bj.entries.iter().rev() {
                for s in e.slots.iter().rev() {
                    restores.push((s.address, s.slot, s.original));
                }
            }
        }
        // Single lock acquisition; later writes (earlier txs, since we reversed) win.
        mirror.apply_batch(restores.iter().copied());
        self.footprint(block)
    }

    /// [B5] Journal-driven MULTI-BLOCK unwind for a feed regression deeper than one block (a reorg).
    /// Exact-inverse rolls back every journaled block strictly newer than `target`, NEWEST block
    /// first (so a slot touched across several unwound blocks composes back to its end-of-`target`
    /// value — each block's own [`rollback_block`](Self::rollback_block) already reverses within the
    /// block), prunes those blocks from the ring, purges their `(block, tx_hash)` idempotency keys,
    /// and resets the head — leaving the mirror at the value-exact END-OF-`target` state, ready for
    /// the advancer to re-apply forward from `target`. Returns the deduped union footprint of the
    /// unwound range (the reconcile candidate set — feed it to [`reconcile_footprint`]).
    ///
    /// If `target` is below the oldest retained block the ring cannot exact-unwind that deep
    /// ([`RollbackTo::BeyondJournal`], **no mutation** — the caller degrades to unhealthy + full
    /// resync). If `target >= head` there is nothing newer to unwind ([`RollbackTo::NoOp`]).
    pub fn rollback_to(&mut self, mirror: &StateMirror, target: u64) -> RollbackTo {
        // Nothing strictly newer than `target` ⇒ not a regression.
        if target >= self.head_block {
            return RollbackTo::NoOp;
        }
        // The ring is a bounded [oldest, head] window. To exact-unwind to end-of-`target` we must
        // still hold every block we invert; if `target` is below the oldest retained block those
        // inverses are gone → cannot exact-unwind. No mutation; the caller degrades + resyncs.
        let oldest = match self.blocks.front() {
            Some(b) => b.block,
            None => return RollbackTo::NoOp, // empty ring (head_block==0 already returned NoOp)
        };
        if target < oldest {
            self.reorg_beyond_journal += 1;
            return RollbackTo::BeyondJournal;
        }
        // Blocks to unwind: everything strictly above `target`. Collect ascending (ring order), then
        // roll back in REVERSE (newest first) so cross-block composition rewinds correctly.
        let mut victims: Vec<u64> = self
            .blocks
            .iter()
            .filter(|b| b.block > target)
            .map(|b| b.block)
            .collect();
        victims.sort_unstable();
        let mut fp_seen: AHashSet<([u8; 20], U256)> = AHashSet::new();
        let mut footprint: Vec<([u8; 20], U256)> = Vec::new();
        let mut blocks_unwound = 0u64;
        for &b in victims.iter().rev() {
            for kv in self.rollback_block(mirror, b) {
                if fp_seen.insert(kv) {
                    footprint.push(kv);
                }
            }
            blocks_unwound += 1;
        }
        // Prune the unwound blocks from the ring + purge their idempotency keys, so a re-delivery of
        // the reorged block re-applies FORWARD (opens a fresh block) instead of being deduped or
        // rejected as out-of-order. NOT counted as `evicted_unreconciled`: these blocks were
        // actively rolled back + will be reconciled, not silently dropped.
        while let Some(back) = self.blocks.back() {
            if back.block <= target {
                break;
            }
            if let Some(dropped) = self.blocks.pop_back() {
                for e in &dropped.entries {
                    self.seen.remove(&(dropped.block, e.tx_hash));
                }
                if let Some(p) = &self.persist {
                    let _ = p.delete_block(dropped.block);
                }
            }
        }
        // Head = the newest SURVIVING block: `target` itself when it was journaled, else the largest
        // journaled block below `target` (a skipped/empty block leaves nothing to invert, and
        // end-of-survivor == end-of-`target` for the journal's footprint). Either way the next
        // forward tx at `cur_block` opens or extends the correct block.
        self.head_block = self.blocks.back().map(|b| b.block).unwrap_or(0);
        self.reorg_unwinds += 1;
        self.reorg_blocks_unwound += blocks_unwound;
        RollbackTo::Unwound { footprint, blocks_unwound }
    }

    /// [WP4 · Fix E stage (a)] PLAN a multi-block unwind to `target` WITHOUT mutating the journal or
    /// the mirror — the fail-closed prerequisite for the checked reorg path: we must validate ring
    /// coverage and compute the affected footprint BEFORE any state is fetched or written, so a
    /// checked-fetch failure can abort with zero mutation (never mutate-then-fetch). Returns the
    /// deduped union footprint of the blocks strictly newer than `target` (the reconcile candidate set)
    /// plus how many blocks would be unwound; [`RollbackTo::BeyondJournal`] when the ring can't reach
    /// that deep; [`RollbackTo::NoOp`] when there is nothing newer to unwind. Pure (read-only).
    pub fn plan_rollback_to(&self, target: u64) -> RollbackTo {
        if target >= self.head_block {
            return RollbackTo::NoOp;
        }
        let oldest = match self.blocks.front() {
            Some(b) => b.block,
            // [FIX-WP5-A] Empty ring but `target < head_block` (the `target >= head_block` NoOp is
            // handled above): there is NO retained history to unwind, so the target is UNREACHABLE —
            // this is BeyondJournal (degrade + full resync), never NoOp. This is the fail-open a warm
            // resume opened: `activate` sets `head_block = anchor` with an EMPTY ring, so a reorg below
            // the anchor would otherwise classify NoOp → retag without latching → the resume-hydrated
            // slots survive and a later clean close publishes them certified though never re-proved.
            None => return RollbackTo::BeyondJournal,
        };
        if target < oldest {
            return RollbackTo::BeyondJournal;
        }
        let mut fp_seen: AHashSet<([u8; 20], U256)> = AHashSet::new();
        let mut footprint: Vec<([u8; 20], U256)> = Vec::new();
        let mut blocks_unwound = 0u64;
        for b in self.blocks.iter().filter(|b| b.block > target) {
            for e in &b.entries {
                for s in &e.slots {
                    if fp_seen.insert((s.address, s.slot)) {
                        footprint.push((s.address, s.slot));
                    }
                }
            }
            blocks_unwound += 1;
        }
        RollbackTo::Unwound { footprint, blocks_unwound }
    }

    /// [WP4 · Fix E stage (c)] COMMIT a planned unwind's JOURNAL side: prune every block strictly newer
    /// than `target` from the ring, purge their `(block, tx_hash)` idempotency keys + persisted rows,
    /// and reset the head to the newest surviving block. Does NOT touch the mirror — the checked reorg
    /// caller applies the verified canonical footprint patch to the mirror separately, then calls this,
    /// so the ring and mirror commit together (all-or-none: called ONLY after every footprint value
    /// verified). Counts one reorg unwind of `blocks_unwound` blocks (the honesty ledger).
    pub fn commit_rollback_prune(&mut self, target: u64, blocks_unwound: u64) {
        while let Some(back) = self.blocks.back() {
            if back.block <= target {
                break;
            }
            if let Some(dropped) = self.blocks.pop_back() {
                for e in &dropped.entries {
                    self.seen.remove(&(dropped.block, e.tx_hash));
                }
                if let Some(p) = &self.persist {
                    let _ = p.delete_block(dropped.block);
                }
            }
        }
        self.head_block = self.blocks.back().map(|b| b.block).unwrap_or(0);
        self.reorg_unwinds += 1;
        self.reorg_blocks_unwound += blocks_unwound;
    }

    /// [WP4 · Fix E] Count a beyond-journal reorg (the ring cannot exact-unwind that deep) in the
    /// honesty ledger. Split from [`plan_rollback_to`](Self::plan_rollback_to) so the plan stays pure.
    pub fn note_reorg_beyond_journal(&mut self) {
        self.reorg_beyond_journal += 1;
    }

    /// [B5] Drop the entire ring (blocks + idempotency keys + head) for a FULL resync — used on the
    /// beyond-journal degrade path, where no exact inverse exists so the journal restarts clean from
    /// the resync head. The cumulative honesty ledgers (`evicted_unreconciled`, `reorg_*`) are
    /// preserved. Persisted blocks (if any) are deleted so a restart does not resume the orphaned
    /// chain. The mirror's storage is untouched here (the caller trips it unhealthy; it re-heals on
    /// the next clean block reconcile).
    pub fn reset(&mut self) {
        if let Some(p) = &self.persist {
            for b in &self.blocks {
                let _ = p.delete_block(b.block);
            }
        }
        self.blocks.clear();
        self.seen.clear();
        self.head_block = 0;
        // [WP1] A full resync re-establishes trust from scratch: the carried proof debt is discharged
        // by the resync itself, never silently dropped mid-flight (that decision is the caller's, made
        // on `full_resync_required`).
        self.debt.clear();
        // [WP1 · FIX-1] NOTE: `full_resync_pending` is deliberately NOT cleared here. reset() only
        // clears the ring + debt bookkeeping; it is NOT "the resync". The sticky latch must outlive
        // reset() so a subsequent clean close cannot re-trust the mirror (debt-laundering). It clears
        // only via `clear_full_resync_pending` (explicit re-hydration / operator resync).
    }

    /// Reconcile one closed block against canonical state (Gate-C guarantee 3). Deep-compares the
    /// mode-selected footprint sample; on ANY mismatch, rolls the whole footprint back, re-fetches
    /// it from `canonical`, re-applies, and marks the block reconciled. A degraded-flagged block is
    /// force-reconciled with the FULL footprint regardless of `mode`.
    ///
    /// `sample_n` bounds the deep-compare (the "audit N", default 64). Repair re-fetches the whole
    /// footprint (correctness over sampling once a divergence is proven).
    pub fn reconcile_block<F: StateFetcher>(
        &mut self,
        mirror: &StateMirror,
        block: u64,
        canonical: &F,
        mode: ReconcileMode,
        sample_n: usize,
    ) -> ReconcileReport {
        let forced = self.is_flagged(block);
        let effective = if forced { ReconcileMode::Logs } else { mode };
        if matches!(effective, ReconcileMode::Off) {
            return ReconcileReport { forced, ..Default::default() };
        }
        let footprint = self.footprint(block);
        if footprint.is_empty() {
            return ReconcileReport { forced, ..Default::default() };
        }
        let sample = select_sample(&footprint, effective, sample_n);
        let mut checked = 0usize;
        let mut mismatched = 0usize;
        for (addr, slot) in &sample {
            checked += 1;
            let mirror_val = mirror.get_storage(*addr, *slot).unwrap_or(U256::ZERO);
            let canon = canonical.fetch_storage(*addr, *slot);
            if mirror_val != canon {
                mismatched += 1;
            }
        }
        let mut repaired = 0usize;
        if mismatched > 0 {
            // Roll the block back, then rewrite the whole footprint from canonical (record-then-
            // repair). Reuses the exact-inverse rollback so the mirror is provably re-based before
            // canonical values land.
            let fp = self.rollback_block(mirror, block);
            let mut fresh: Vec<([u8; 20], U256, U256)> = Vec::with_capacity(fp.len());
            for (addr, slot) in &fp {
                fresh.push((*addr, *slot, canonical.fetch_storage(*addr, *slot)));
            }
            repaired = fresh.len();
            mirror.apply_batch(fresh.into_iter());
        }
        // Block is now provably head-consistent for its footprint (either it matched, or it was
        // repaired). Clear the degraded flag.
        if let Some(bj) = self.block_mut(block) {
            bj.reconcile_flagged = false;
        }
        ReconcileReport { checked, mismatched, repaired, forced }
    }

    /// [WP1] The outstanding reconciliation debt (test/observability + full-resync decisions).
    pub fn debt(&self) -> &ReconcileDebt {
        &self.debt
    }

    /// [WP1 · Fix B2] Checked, transactional reconcile of one closed block. This is the fail-closed
    /// replacement for [`reconcile_block`](Self::reconcile_block): it consumes the CHECKED seam
    /// ([`StateFetcher::fetch_storage_checked`]) so an unavailable/invalid proof or an untrusted anchor
    /// can never enter a value into the mirror. Two-phase, all-or-none:
    ///
    /// 1. Stage the mode-selected footprint sample UNIONED with all carried debt (debt is re-proved at
    ///    the CURRENT block `block`, not its historical failure block).
    /// 2. If ANY staged fetch fails: perform NO mirror mutation, union the full affected footprint
    ///    (this block's footprint + carried debt) into [`ReconcileDebt`], keep/raise the reconcile
    ///    flag, and return `complete=false, publishable=false`. A failed fetch is NEVER a mismatch.
    /// 3. Compare only successfully staged values. Clean + all debt re-proved ⇒ clear debt + flag,
    ///    publishable.
    /// 4. On mismatch, stage the COMPLETE repair set (full footprint + debt) BEFORE any write; on a
    ///    single failure, zero mutation + debt (step 2). Otherwise commit ONE all-or-none canonical
    ///    patch (a direct full-footprint write — never a rollback+refetch), then clear debt + flag.
    ///
    /// `deadline` (carry-over d) bounds total fetch wall-clock for this operation; exhaustion is a
    /// canonical failure (fail-closed), never a silent skip.
    pub fn reconcile_block_checked<F: StateFetcher>(
        &mut self,
        mirror: &StateMirror,
        block: u64,
        canonical: &F,
        mode: ReconcileMode,
        sample_n: usize,
        deadline: Option<std::time::Instant>,
    ) -> ReconcileOutcome {
        let forced = self.is_flagged(block);
        let effective = if forced { ReconcileMode::Logs } else { mode };
        let footprint = self.footprint(block);
        let debt_slots = self.debt.slots();

        // Audit = mode-selected footprint sample UNION every carried debt slot (re-proved here).
        let mut audit = select_sample(&footprint, effective, sample_n);
        let mut seen: AHashSet<([u8; 20], U256)> = audit.iter().copied().collect();
        for kv in &debt_slots {
            if seen.insert(*kv) {
                audit.push(*kv);
            }
        }

        if audit.is_empty() {
            // Nothing entered the checked boundary (Off / empty footprint) and no debt to discharge.
            return ReconcileOutcome {
                forced,
                complete: true,
                publishable: self.debt.is_empty(),
                ..Default::default()
            };
        }

        // Phase 1: stage the audit set through the checked seam.
        let staged = match stage_checked(canonical, &audit, deadline) {
            Ok(m) => m,
            Err((failures, last_err)) => {
                return self.open_debt_and_fail(block, &footprint, &debt_slots, forced, failures, last_err)
            }
        };

        // Phase 2: compare ONLY successfully staged values (a failed fetch is never a mismatch).
        let mut mismatched = 0usize;
        for (kv, canon) in &staged {
            if mirror.get_storage(kv.0, kv.1).unwrap_or(U256::ZERO) != canon.get() {
                mismatched += 1;
            }
        }
        let checked = staged.len();

        if mismatched == 0 {
            // Clean AND every carried debt slot was re-proved this pass ⇒ discharge debt + flag.
            self.debt.clear();
            self.clear_flag(block);
            return ReconcileOutcome {
                checked,
                forced,
                complete: true,
                publishable: true,
                ..Default::default()
            };
        }

        // Phase 3: stage the COMPLETE repair set (full footprint + carried debt) BEFORE any write.
        let mut repair_slots = footprint.clone();
        let mut seen2: AHashSet<([u8; 20], U256)> = repair_slots.iter().copied().collect();
        for kv in &debt_slots {
            if seen2.insert(*kv) {
                repair_slots.push(*kv);
            }
        }
        let need: Vec<([u8; 20], U256)> = repair_slots
            .iter()
            .copied()
            .filter(|kv| !staged.contains_key(kv))
            .collect();
        let extra = match stage_checked(canonical, &need, deadline) {
            Ok(m) => m,
            Err((failures, last_err)) => {
                return self.open_debt_and_fail(block, &footprint, &debt_slots, forced, failures, last_err)
            }
        };

        // Every repair slot is now resolved (in `staged` or `extra`). Build ONE all-or-none patch of
        // PROVEN values — committed through the checked boundary (`apply_checked_batch`), so an
        // unproven value is type-impossible to apply here.
        let mut patch: Vec<([u8; 20], U256, Proven<U256>)> = Vec::with_capacity(repair_slots.len());
        for kv in &repair_slots {
            if let Some(v) = staged.get(kv).copied().or_else(|| extra.get(kv).copied()) {
                patch.push((kv.0, kv.1, v));
            }
        }
        let repaired = patch.len();
        mirror.apply_checked_batch(patch.into_iter());
        self.debt.clear();
        self.clear_flag(block);
        ReconcileOutcome {
            checked,
            mismatched,
            repaired,
            forced,
            complete: true,
            publishable: true,
            ..Default::default()
        }
    }

    /// [WP1 · Fix B2 step 4] Fail-closed: perform no mutation, union the full affected footprint into
    /// debt, keep/raise the reconcile flag, and report the incomplete outcome (escalating to a full
    /// resync if debt exceeds its size/age cap — never a silent eviction).
    fn open_debt_and_fail(
        &mut self,
        block: u64,
        footprint: &[([u8; 20], U256)],
        debt_slots: &[([u8; 20], U256)],
        forced: bool,
        failures: usize,
        last_err: Option<CanonicalFetchErrorKind>,
    ) -> ReconcileOutcome {
        let affected: Vec<([u8; 20], U256)> =
            footprint.iter().chain(debt_slots.iter()).copied().collect();
        self.debt.add(affected, block, last_err);
        // Keep/raise the block's forced-reconcile flag (an incomplete pass never clears it).
        self.flag_reconcile(block);
        let over_cap = self.debt.len() > MAX_DEBT_SLOTS;
        let over_age = self.debt.since_block != 0
            && block.saturating_sub(self.debt.since_block) > MAX_DEBT_AGE_BLOCKS;
        if over_cap || over_age {
            // [FIX-1] STICKY: latch the mirror untrusted-until-resync. This survives reset() and any
            // subsequent clean close — the escalated slots were never proven, so no plain clean sample
            // may re-trust the mirror.
            self.full_resync_pending = true;
        }
        ReconcileOutcome {
            forced,
            canonical_failures: failures,
            complete: false,
            publishable: false,
            unresolved: self.debt.slots(),
            full_resync_required: over_cap || over_age,
            last_error: last_err,
            ..Default::default()
        }
    }

    /// [WP1 · FIX-1] Whether a full-resync latch is pending (set on debt-cap escalation; sticky — a
    /// clean close cannot clear it). The advancer health verdict + publish gate treat this as a
    /// dominating "untrusted" input.
    pub fn full_resync_pending(&self) -> bool {
        self.full_resync_pending
    }

    /// [WP1 · FIX-1] Set the sticky full-resync latch (escalation site + tests/tooling). Once set, only
    /// [`clear_full_resync_pending`](Self::clear_full_resync_pending) clears it.
    pub fn set_full_resync_pending(&mut self) {
        self.full_resync_pending = true;
    }

    /// [WP1 · FIX-1 → WP5 · WP4 carry-over] Clear the sticky full-resync latch. Its FIRST in-process
    /// caller is [`recover_full_resync`](Self::recover_full_resync): an escalated mirror can now recover
    /// IN-RUN once its whole resident footprint re-proves at a current checked anchor (before that the
    /// latch could clear only by process restart). Still never cleared by a plain clean close.
    pub fn clear_full_resync_pending(&mut self) {
        self.full_resync_pending = false;
    }

    /// [WP5 · F4] Install a fully-validated [`ActivatedResume`] — the ONLY path by which persisted data
    /// affects trust. Hydrates the fresh mirror with the proven anchor patch (all-or-none, through the
    /// checked boundary), retags it to the anchor, sets the journal head, and publishes a CERTIFIED
    /// snapshot. Pragmatic (spec-sanctioned) scope: the rollback RING is NOT restored — warm-resume
    /// sacrifices exact-inverse unwind depth into pre-restart blocks (a later reorg below the anchor
    /// simply degrades to a full resync), because boundary values alone do not prove tx-membership
    /// inverses. Clears `RESUME_PENDING` last (activation succeeded).
    pub fn activate(&mut self, mirror: &StateMirror, resume: ActivatedResume) {
        let anchor = resume.anchor_block;
        // [FIX-AUDIT-1(a)] Fold the resume footprint into reconciliation DEBT before publishing. The
        // hydration was proven at the anchor, but a SAME-NUMBER reorg of the anchor block during the
        // restart window is invisible to number-only feed-advance classification — so the FIRST forward
        // close must RE-PROVE the resume footprint at the current block (debt slots are always re-proved
        // in `reconcile_block_checked`, catching an anchor reorg). Until it re-proves, the mirror is
        // untrusted (PROOF_UNRESOLVED) and — [FIX-AUDIT-3] — NOT published.
        let footprint: Vec<([u8; 20], U256)> =
            resume.patch.iter().map(|(a, s, _)| (*a, *s)).collect();
        mirror.apply_checked_batch(resume.patch.into_iter());
        mirror.retag_block(anchor);
        self.head_block = anchor;
        if !footprint.is_empty() {
            self.debt.add(footprint, anchor, None);
            mirror.set_reason(crate::state_mirror::health_reason::PROOF_UNRESOLVED);
        }
        mirror.clear_reason(crate::state_mirror::health_reason::RESUME_PENDING);
        // [FIX-AUDIT-3] Publish only a fully reason-free mirror — symmetric with every other publish
        // site. With the resume footprint in debt this withholds the (possibly-reorged) anchor snapshot
        // until the first close re-proves it and heals PROOF_UNRESOLVED.
        if mirror.is_healthy() {
            mirror.publish_certified(anchor);
        }
    }

    /// [WP5 · WP4 carry-over] In-run recovery from a sticky full-resync escalation (a WP1 debt-cap
    /// escalation or a WP4 reorg failure). Re-proves the mirror's ENTIRE resident footprint at a current
    /// checked anchor, all-or-none. On complete success it clears the sticky latch and returns `true`
    /// (the caller then clears `FULL_RESYNC_REQUIRED` and may re-publish); on any unproven slot it
    /// leaves the latch set and returns `false` (stays untrusted, retries later). This is the first
    /// in-process path that can lift `FULL_RESYNC_REQUIRED` without a restart. Cost: O(resident slots)
    /// proof fetches — a rare recovery, bounded by `deadline`.
    pub fn recover_full_resync<F: StateFetcher>(
        &mut self,
        mirror: &StateMirror,
        canonical: &F,
        deadline: Option<std::time::Instant>,
    ) -> bool {
        let footprint = mirror.resident_footprint();
        let out = reconcile_footprint_checked(mirror, &footprint, canonical, deadline);
        if out.complete {
            self.clear_full_resync_pending();
            // [FIX-AUDIT-2] A complete re-prove of the WHOLE resident footprint SUBSUMES the reorg
            // footprint, so it clears BOTH the sticky full-resync latch AND REORG_RECOVERY — a
            // reorg-degraded mirror recovers in-run instead of staying stuck untrusted until another
            // reorg/restart. (These are the only reasons a full re-prove is authoritative over; a
            // QUEUE_GAP etc. is cleared by its own owner.)
            mirror.clear_reason(
                crate::state_mirror::health_reason::FULL_RESYNC_REQUIRED
                    | crate::state_mirror::health_reason::REORG_RECOVERY,
            );
            true
        } else {
            false
        }
    }

    /// Clear a block's forced-reconcile flag (only on a complete, clean/repaired pass).
    fn clear_flag(&mut self, block: u64) {
        if let Some(bj) = self.block_mut(block) {
            bj.reconcile_flagged = false;
        }
    }
}

/// Deterministic footprint selection for reconcile. `Logs` → first `sample_n` in canonical order;
/// `Sampled` → the `sample_n` lowest-hash slots (a stable, reproducible fraction; Fulcrum-style
/// deterministic sampling). Both are subsets of `footprint`, both stable across identical inputs.
fn select_sample(
    footprint: &[([u8; 20], U256)],
    mode: ReconcileMode,
    sample_n: usize,
) -> Vec<([u8; 20], U256)> {
    let n = sample_n.max(1);
    match mode {
        ReconcileMode::Off => Vec::new(),
        ReconcileMode::Logs => footprint.iter().take(n).copied().collect(),
        ReconcileMode::Sampled => {
            let mut scored: Vec<(u64, ([u8; 20], U256))> =
                footprint.iter().map(|kv| (sample_hash(kv), *kv)).collect();
            // Stable order by hash then key, so equal-hash ties are deterministic.
            scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1 .0.cmp(&b.1 .0)));
            scored.into_iter().take(n).map(|(_, kv)| kv).collect()
        }
    }
}

/// FNV-1a over `address || slot` — a cheap, deterministic per-slot hash for sampling selection.
fn sample_hash(kv: &([u8; 20], U256)) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    mix(&kv.0);
    mix(&kv.1.to_be_bytes::<32>());
    h
}

/// [B5] Reconcile an explicit `footprint` (e.g. the union [`MirrorJournal::rollback_to`] returns)
/// against canonical state, repairing every slot that diverges. Used AFTER a multi-block unwind to
/// PROVE the exact-inverse re-based mirror equals canonical-end-of-target across the whole unwound
/// range — and to repair any residual drift a pre-existing gap left behind. Full compare (no
/// sampling): a reorg is rare and correctness dominates. Repaired values enter the mirror as the
/// head-fresh canonical set (same [`StateMirror::apply_batch`] path as [`reconcile_block`]).
pub(crate) fn reconcile_footprint<F: StateFetcher>(
    mirror: &StateMirror,
    footprint: &[([u8; 20], U256)],
    canonical: &F,
) -> ReconcileReport {
    let mut checked = 0usize;
    let mut mismatched = 0usize;
    let mut fixes: Vec<([u8; 20], U256, U256)> = Vec::new();
    for (addr, slot) in footprint {
        checked += 1;
        let mirror_val = mirror.get_storage(*addr, *slot).unwrap_or(U256::ZERO);
        let canon = canonical.fetch_storage(*addr, *slot);
        if mirror_val != canon {
            mismatched += 1;
            fixes.push((*addr, *slot, canon));
        }
    }
    let repaired = fixes.len();
    if repaired > 0 {
        mirror.apply_batch(fixes.into_iter());
    }
    ReconcileReport { checked, mismatched, repaired, forced: false }
}

/// [WP1] Stage every `slots` value through the CHECKED seam ([`StateFetcher::fetch_storage_checked`])
/// into a map, or report `(failures, last_reason)` on ANY failure. This is the shared checked core
/// reused by normal close ([`MirrorJournal::reconcile_block_checked`]) and, in WP4, deep-reorg
/// footprint recovery.
///
/// `deadline` budget semantics (carry-over d), stated honestly: the deadline is checked ONCE PER SLOT
/// BEFORE its fetch — it does NOT interrupt a fetch already in flight. So the true wall-clock bound is
/// `budget + one per-request read timeout` (the last fetch that starts just under the deadline may run
/// to the RPC client's `RPC_TIMEOUT_READ_SECS`). Once the deadline passes, every remaining slot is a
/// canonical failure (fail-closed) rather than a silent skip. When the budget is disabled
/// (`MIRROR_RECONCILE_BUDGET_MS=0` ⇒ `deadline = None`) there is NO aggregate cap: a slow/hung provider
/// can stall the advancer for up to `slots × read_timeout`, so operators enabling a large footprint
/// with `=0` accept that stall risk.
fn stage_checked<F: StateFetcher>(
    canonical: &F,
    slots: &[([u8; 20], U256)],
    deadline: Option<std::time::Instant>,
) -> Result<AHashMap<([u8; 20], U256), Proven<U256>>, (usize, Option<CanonicalFetchErrorKind>)> {
    let mut staged = AHashMap::with_capacity(slots.len());
    let mut failures = 0usize;
    let mut last_err: Option<CanonicalFetchErrorKind> = None;
    for kv in slots {
        if deadline.map(|d| std::time::Instant::now() >= d).unwrap_or(false) {
            // Operation budget exhausted ⇒ the remaining values cannot be proven in time.
            failures += 1;
            last_err = last_err.or(Some(CanonicalFetchErrorKind::ProofUnavailable));
            continue;
        }
        match canonical.fetch_storage_checked(kv.0, kv.1) {
            // [WP4 · Fix E] This is the SOLE construction site of `Proven` — the value has just flowed
            // through the checked seam, so it may be committed to the mirror; a raw `fetch_storage`
            // value never reaches here.
            Ok(v) => {
                staged.insert(*kv, Proven::checked(v));
            }
            Err(e) => {
                failures += 1;
                last_err = Some(e.kind);
            }
        }
    }
    if failures > 0 {
        Err((failures, last_err))
    } else {
        Ok(staged)
    }
}

/// [WP4 · checked reorg core] Checked, all-or-none reconcile of an explicit `footprint` (the union a
/// multi-block reorg unwind returns). Stages the WHOLE footprint through the checked seam BEFORE any
/// mutation; on ANY canonical failure it writes nothing and returns `complete=false` with the full
/// footprint as unresolved. Otherwise it commits one canonical patch. This is the checked deep-reorg
/// path's fail-closed core — wired at `shadow_revm::close`'s `DeepRegress` branch under
/// `MIRROR_COLD_VERIFY=1`, and the same fail-closed core as normal-close reconcile.
pub(crate) fn reconcile_footprint_checked<F: StateFetcher>(
    mirror: &StateMirror,
    footprint: &[([u8; 20], U256)],
    canonical: &F,
    deadline: Option<std::time::Instant>,
) -> ReconcileOutcome {
    match stage_checked(canonical, footprint, deadline) {
        Err((failures, last_err)) => ReconcileOutcome {
            canonical_failures: failures,
            complete: false,
            publishable: false,
            unresolved: footprint.to_vec(),
            last_error: last_err,
            ..Default::default()
        },
        Ok(staged) => {
            let mut mismatched = 0usize;
            let mut patch: Vec<([u8; 20], U256, Proven<U256>)> = Vec::with_capacity(footprint.len());
            for kv in footprint {
                // [WP4 · FIX-A/L1] `stage_checked` returned Ok ⇒ it staged EVERY footprint slot (it
                // was called on exactly `footprint`). A miss is an invariant violation — fail LOUD, not
                // a fabricated proven ZERO (which would be a latent fail-open if the Ok-postcondition
                // ever weakened). `Proven::checked` stays the SOLE construction site (stage_checked).
                let canon = *staged
                    .get(kv)
                    .expect("stage_checked Ok ⇒ every footprint slot staged");
                if mirror.get_storage(kv.0, kv.1).unwrap_or(U256::ZERO) != canon.get() {
                    mismatched += 1;
                }
                patch.push((kv.0, kv.1, canon));
            }
            let repaired = if mismatched > 0 {
                let n = patch.len();
                // [WP4] Commit through the CHECKED boundary: only proven values reach the mirror.
                mirror.apply_checked_batch(patch.into_iter());
                n
            } else {
                0
            };
            ReconcileOutcome {
                checked: footprint.len(),
                mismatched,
                repaired,
                complete: true,
                publishable: true,
                ..Default::default()
            }
        }
    }
}

// ── WP5 · Fix F4: canonical resume recovery (validate → hydrate → activate) ───────────────────────

use crate::mirror_journal_persist::PersistedBlock;

/// [WP5 · F4] Why a persisted resume candidate could not be safely activated. On ANY of these the
/// candidate is discarded/quarantined and the process cold/full-resyncs — never a partial activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeError {
    /// No retained block matches canonical truth (all orphaned by a restart-spanning reorg, or the
    /// persisted head is entirely above the live head).
    NoCommonAncestor,
    /// A block within the accepted prefix is not canonical (orphaned) — cannot trust the prefix.
    OrphanInPrefix(u64),
    /// The retained prefix is not contiguous / its parent links do not chain.
    BrokenChain(u64),
    /// A canonical header could not be fetched (fail-closed — never treated as a match).
    HeaderUnavailable(u64),
    /// The footprint could not be fully proven at the accepted anchor (all-or-none hydration failed).
    HydrationIncomplete,
    /// Nothing to activate (empty candidate).
    Empty,
}

/// [WP5 · F4] A fully-validated resume ready to install: the accepted canonical anchor and the
/// all-or-none, PROVEN hydration patch fetched at that anchor. Produced only after every validation
/// step succeeds; consumed by [`MirrorJournal::activate`].
#[derive(Debug)]
pub struct ActivatedResume {
    /// The highest retained block proven canonical — the resume head.
    pub anchor_block: u64,
    /// The deduplicated resident footprint, each value proven canonical at `anchor_block`.
    pub patch: Vec<([u8; 20], U256, Proven<U256>)>,
    /// How many retained blocks formed the accepted canonical prefix (for logs/metrics).
    pub prefix_blocks: usize,
}

/// [WP5 · F4] Stage a footprint through the checked seam WITHOUT mutating any mirror — returns the
/// all-or-none PROVEN patch (or the failure count). Reuses the same `stage_checked` core as normal
/// reconcile, so a resume hydration is proven exactly like every other S5 write.
pub fn stage_footprint_checked<F: StateFetcher>(
    footprint: &[([u8; 20], U256)],
    canonical: &F,
    deadline: Option<std::time::Instant>,
) -> Result<Vec<([u8; 20], U256, Proven<U256>)>, (usize, Option<CanonicalFetchErrorKind>)> {
    let staged = stage_checked(canonical, footprint, deadline)?;
    let mut patch = Vec::with_capacity(footprint.len());
    for kv in footprint {
        let v = *staged
            .get(kv)
            .expect("stage_checked Ok ⇒ every footprint slot staged");
        patch.push((kv.0, kv.1, v));
    }
    Ok(patch)
}

/// [WP5 · F4] Validate a loaded resume candidate against canonical truth and stage its hydration —
/// WITHOUT mutating any state. Steps: (1) select the highest retained block whose persisted hash
/// matches canonical AND that sits at/below `live_head` (a restart-spanning reorg drops orphans above
/// it; a persisted head entirely above live fails safe as `NoCommonAncestor`); (2) prove the accepted
/// prefix `[..=anchor]` canonical + contiguous (parent links chain, numbers consecutive, each hash
/// matches canonical); (3) union the prefix's changed slots into a deduplicated footprint; (4) stage
/// that footprint through the checked/proven `make_fetcher(anchor)` — all-or-none. Any failure ⇒
/// `ResumeError` (discard). `header_hash(n)` returns the canonical block hash at `n` (fail-closed).
pub fn recover_and_validate<H, F, MK>(
    blocks: &[PersistedBlock],
    live_head: u64,
    header_hash: H,
    make_fetcher: MK,
    deadline: Option<std::time::Instant>,
) -> Result<ActivatedResume, ResumeError>
where
    H: Fn(u64) -> Result<[u8; 32], ResumeError>,
    F: StateFetcher,
    MK: Fn(u64) -> F,
{
    if blocks.is_empty() {
        return Err(ResumeError::Empty);
    }
    // The candidate is ascending + unique (load_candidate guarantees it). Select the anchor: the
    // highest retained block at/below the live head whose persisted hash matches canonical truth.
    let mut anchor_idx = None;
    for i in (0..blocks.len()).rev() {
        let b = &blocks[i];
        if b.block > live_head {
            continue; // above the live head ⇒ cannot be canonical yet (orphan / persisted-head-above-live)
        }
        match header_hash(b.block) {
            Ok(h) if h == b.block_hash => {
                anchor_idx = Some(i);
                break;
            }
            // A header we cannot fetch, or a hash mismatch (orphan), is never accepted as the anchor —
            // walk down toward a retained common ancestor.
            _ => continue,
        }
    }
    let ai = anchor_idx.ok_or(ResumeError::NoCommonAncestor)?;

    // Prove the accepted prefix [0..=ai] canonical + contiguous.
    for i in 0..=ai {
        let b = &blocks[i];
        let canon = header_hash(b.block).map_err(|_| ResumeError::HeaderUnavailable(b.block))?;
        if canon != b.block_hash {
            return Err(ResumeError::OrphanInPrefix(b.block));
        }
        if i > 0 {
            let prev = &blocks[i - 1];
            if b.block != prev.block + 1 {
                return Err(ResumeError::BrokenChain(b.block));
            }
            if b.parent_hash != prev.block_hash {
                return Err(ResumeError::BrokenChain(b.block));
            }
        }
    }
    let anchor_block = blocks[ai].block;

    // Deduplicated resident footprint of the accepted prefix.
    let mut seen: AHashSet<([u8; 20], U256)> = AHashSet::new();
    let mut footprint: Vec<([u8; 20], U256)> = Vec::new();
    for b in &blocks[..=ai] {
        for e in &b.entries {
            for s in &e.slots {
                if seen.insert((s.address, s.slot)) {
                    footprint.push((s.address, s.slot));
                }
            }
        }
    }

    // Hydrate at ONE accepted anchor through the checked/proven path — all-or-none.
    let fetcher = make_fetcher(anchor_block);
    let patch = stage_footprint_checked(&footprint, &fetcher, deadline)
        .map_err(|_| ResumeError::HydrationIncomplete)?;
    Ok(ActivatedResume {
        anchor_block,
        patch,
        prefix_blocks: ai + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(addr: [u8; 20], slot: u64, original: u64, present: u64) -> ChangedSlot {
        ChangedSlot {
            address: addr,
            slot: U256::from(slot),
            original: U256::from(original),
            present: U256::from(present),
        }
    }

    fn txh(b: u8) -> [u8; 32] {
        [b; 32]
    }

    /// A canonical `StateFetcher` backed by an explicit slot map (no network) — the reconcile's
    /// ground truth in tests.
    struct MapFetcher {
        slots: ahash::AHashMap<([u8; 20], U256), U256>,
    }
    impl MapFetcher {
        fn new() -> Self {
            Self { slots: ahash::AHashMap::new() }
        }
        fn with(mut self, addr: [u8; 20], slot: u64, val: u64) -> Self {
            self.slots.insert((addr, U256::from(slot)), U256::from(val));
            self
        }
    }
    impl StateFetcher for MapFetcher {
        fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (U256::ZERO, 0, None)
        }
        fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
            self.slots.get(&(a, slot)).copied().unwrap_or(U256::ZERO)
        }
    }

    /// Seed the mirror with a slot's pre-block ("pinned") value so `ChangedSlot.original` is
    /// consistent with the mirror — exactly the fork↔mirror byte-identity the advancer maintains.
    fn seed(mirror: &StateMirror, addr: [u8; 20], slot: u64, val: u64) {
        mirror.apply_batch([(addr, U256::from(slot), U256::from(val))]);
    }

    // ── Gate-C guarantee 2: apply → rollback == identity ──────────────────────────────────────
    #[test]
    fn apply_then_rollback_is_identity_single_tx() {
        let m = StateMirror::new();
        let pool = [0x11; 20];
        seed(&m, pool, 0, 100); // pinned pre-value = the tx's `original`
        let mut j = MirrorJournal::new(8);
        assert_eq!(
            j.record_and_apply(&m, 500, txh(1), &[cs(pool, 0, 100, 777)]),
            ApplyResult::Applied
        );
        assert_eq!(m.get_storage(pool, U256::from(0u64)), Some(U256::from(777u64)));
        j.rollback_block(&m, 500);
        assert_eq!(
            m.get_storage(pool, U256::from(0u64)),
            Some(U256::from(100u64)),
            "rollback restores the block's start-of-block value"
        );
    }

    #[test]
    fn apply_then_rollback_is_identity_composed_same_slot() {
        // Two txs move the SAME slot in-block: rollback (reverse order) must restore the ORIGINAL
        // start-of-block value, not the intermediate one.
        let m = StateMirror::new();
        let pool = [0x22; 20];
        seed(&m, pool, 3, 10);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 700, txh(1), &[cs(pool, 3, 10, 20)]); // 10 → 20
        j.record_and_apply(&m, 700, txh(2), &[cs(pool, 3, 20, 30)]); // 20 → 30
        assert_eq!(m.get_storage(pool, U256::from(3u64)), Some(U256::from(30u64)));
        j.rollback_block(&m, 700);
        assert_eq!(
            m.get_storage(pool, U256::from(3u64)),
            Some(U256::from(10u64)),
            "reverse-order rollback yields the earliest original"
        );
    }

    // ── Gate-C guarantee 1: idempotency + ordering ────────────────────────────────────────────
    #[test]
    fn idempotent_reapply_is_noop() {
        let m = StateMirror::new();
        let pool = [0x33; 20];
        seed(&m, pool, 1, 5);
        let mut j = MirrorJournal::new(8);
        assert_eq!(
            j.record_and_apply(&m, 900, txh(7), &[cs(pool, 1, 5, 55)]),
            ApplyResult::Applied
        );
        assert_eq!(
            j.record_and_apply(&m, 900, txh(7), &[cs(pool, 1, 5, 55)]),
            ApplyResult::DuplicateSkipped,
            "same (block, tx_hash) is idempotent"
        );
        assert_eq!(j.entry_count(), 1, "duplicate did not grow the journal");
        assert_eq!(m.get_storage(pool, U256::from(1u64)), Some(U256::from(55u64)));
    }

    #[test]
    fn out_of_order_older_block_rejected() {
        let m = StateMirror::new();
        let pool = [0x44; 20];
        seed(&m, pool, 0, 1);
        let mut j = MirrorJournal::new(8);
        assert_eq!(
            j.record_and_apply(&m, 100, txh(1), &[cs(pool, 0, 1, 2)]),
            ApplyResult::Applied
        );
        // A tx tagged for an older block than head must be rejected, mirror untouched by it.
        assert_eq!(
            j.record_and_apply(&m, 99, txh(2), &[cs(pool, 0, 500, 600)]),
            ApplyResult::OutOfOrderRejected
        );
        assert_eq!(j.entry_count(), 1);
        assert_eq!(
            m.get_storage(pool, U256::from(0u64)),
            Some(U256::from(2u64)),
            "rejected out-of-order tx left no trace"
        );
        assert_eq!(j.head_block(), 100);
    }

    #[test]
    fn same_block_extends_not_rejected() {
        let m = StateMirror::new();
        let pool = [0x45; 20];
        let mut j = MirrorJournal::new(8);
        assert_eq!(
            j.record_and_apply(&m, 100, txh(1), &[cs(pool, 0, 0, 1)]),
            ApplyResult::Applied
        );
        assert_eq!(
            j.record_and_apply(&m, 100, txh(2), &[cs(pool, 1, 0, 1)]),
            ApplyResult::Applied,
            "another tx in the same block extends it"
        );
        assert_eq!(j.entry_count(), 2);
    }

    // ── Ring eviction bounds RAM; purges idempotency keys ─────────────────────────────────────
    #[test]
    fn ring_evicts_oldest_block() {
        let m = StateMirror::new();
        let pool = [0x55; 20];
        let mut j = MirrorJournal::new(2);
        j.record_and_apply(&m, 1, txh(1), &[cs(pool, 0, 0, 1)]);
        j.record_and_apply(&m, 2, txh(2), &[cs(pool, 0, 1, 2)]);
        j.record_and_apply(&m, 3, txh(3), &[cs(pool, 0, 2, 3)]);
        assert_eq!(j.head_block(), 3);
        assert_eq!(j.block_count(), 2, "ring capped at 2 blocks");
        assert!(j.footprint(1).is_empty(), "block 1 evicted");
        assert!(
            !j.contains(1, &txh(1)),
            "evicted block's idempotency keys are purged"
        );
        assert!(j.contains(3, &txh(3)));
    }

    // ── Footprint dedups repeated (addr, slot) ────────────────────────────────────────────────
    #[test]
    fn footprint_union_dedups() {
        let m = StateMirror::new();
        let a = [0x66; 20];
        let b = [0x67; 20];
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 10, txh(1), &[cs(a, 0, 0, 1), cs(b, 5, 0, 1)]);
        j.record_and_apply(&m, 10, txh(2), &[cs(a, 0, 1, 2)]); // touches a/slot0 again
        let fp = j.footprint(10);
        assert_eq!(fp.len(), 2, "duplicate (addr, slot) collapses");
        assert!(fp.contains(&(a, U256::from(0u64))));
        assert!(fp.contains(&(b, U256::from(5u64))));
    }

    // ── Gate-C guarantee 3: reconcile detects + repairs divergence ────────────────────────────
    #[test]
    fn reconcile_clean_when_mirror_matches_canonical() {
        let m = StateMirror::new();
        let pool = [0x77; 20];
        seed(&m, pool, 0, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(pool, 0, 1, 9)]); // mirror slot0 = 9
        let canon = MapFetcher::new().with(pool, 0, 9); // canonical agrees
        let r = j.reconcile_block(&m, 42, &canon, ReconcileMode::Logs, 64);
        assert_eq!(r.checked, 1);
        assert_eq!(r.mismatched, 0);
        assert_eq!(r.repaired, 0);
        assert_eq!(m.get_storage(pool, U256::from(0u64)), Some(U256::from(9u64)));
    }

    #[test]
    fn reconcile_repairs_divergent_slot() {
        let m = StateMirror::new();
        let pool = [0x88; 20];
        seed(&m, pool, 0, 1);
        let mut j = MirrorJournal::new(8);
        // The mirror applies a WRONG post-value (simulating a missed intervening feed tx).
        j.record_and_apply(&m, 42, txh(1), &[cs(pool, 0, 1, 999)]);
        assert_eq!(m.get_storage(pool, U256::from(0u64)), Some(U256::from(999u64)));
        // Canonical chain says the slot is actually 42.
        let canon = MapFetcher::new().with(pool, 0, 42);
        let r = j.reconcile_block(&m, 42, &canon, ReconcileMode::Logs, 64);
        assert_eq!(r.mismatched, 1);
        assert!(r.repaired >= 1);
        assert_eq!(
            m.get_storage(pool, U256::from(0u64)),
            Some(U256::from(42u64)),
            "mirror repaired to canonical"
        );
    }

    #[test]
    fn reconcile_off_is_noop() {
        let m = StateMirror::new();
        let pool = [0x89; 20];
        seed(&m, pool, 0, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(pool, 0, 1, 999)]);
        let canon = MapFetcher::new().with(pool, 0, 42);
        let r = j.reconcile_block(&m, 42, &canon, ReconcileMode::Off, 64);
        assert_eq!(r, ReconcileReport { checked: 0, mismatched: 0, repaired: 0, forced: false });
        assert_eq!(
            m.get_storage(pool, U256::from(0u64)),
            Some(U256::from(999u64)),
            "Off mode leaves the mirror as-is"
        );
    }

    // ── Gate-C guarantee 4: degraded-flagged block is force-reconciled even in Sampled/Off intent
    #[test]
    fn flagged_block_forces_full_reconcile() {
        let m = StateMirror::new();
        let pool = [0x8a; 20];
        seed(&m, pool, 0, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(pool, 0, 1, 999)]);
        j.flag_reconcile(42);
        assert!(j.is_flagged(42));
        let canon = MapFetcher::new().with(pool, 0, 7);
        // Even with Off intent, a flagged block is force-reconciled (guarantee 4).
        let r = j.reconcile_block(&m, 42, &canon, ReconcileMode::Off, 64);
        assert!(r.forced);
        assert_eq!(r.mismatched, 1);
        assert_eq!(m.get_storage(pool, U256::from(0u64)), Some(U256::from(7u64)));
        assert!(!j.is_flagged(42), "flag cleared once reconciled");
    }

    // ── Sampled selection is deterministic + a subset ─────────────────────────────────────────
    #[test]
    fn sampled_selection_is_deterministic_subset() {
        let mut fp: Vec<([u8; 20], U256)> = Vec::new();
        for i in 0..20u8 {
            fp.push(([i; 20], U256::from(i as u64)));
        }
        let a = select_sample(&fp, ReconcileMode::Sampled, 5);
        let b = select_sample(&fp, ReconcileMode::Sampled, 5);
        assert_eq!(a, b, "same input → same sample");
        assert_eq!(a.len(), 5);
        for kv in &a {
            assert!(fp.contains(kv), "sample ⊆ footprint");
        }
    }

    // ── "Recorded-feed vs fork" essence: ordered full application composes to the reference ───
    #[test]
    fn ordered_full_application_matches_reference_composition() {
        // An ordered multi-tx stream where each tx reads the previous post-state. Applying the FULL
        // stream in order must land the mirror on the hand-computed composed state at block close —
        // the laptop-deterministic analogue of the recorded-feed-vs-anvil-fork slot-equality check.
        let m = StateMirror::new();
        let pa = [0x01; 20];
        let pb = [0x02; 20];
        seed(&m, pa, 0, 1000);
        seed(&m, pb, 0, 500);
        let mut j = MirrorJournal::new(8);
        // block 300: tx1 pa 1000→900, tx2 pb 500→650, tx3 pa 900→870 (composes on tx1)
        j.record_and_apply(&m, 300, txh(1), &[cs(pa, 0, 1000, 900)]);
        j.record_and_apply(&m, 300, txh(2), &[cs(pb, 0, 500, 650)]);
        j.record_and_apply(&m, 300, txh(3), &[cs(pa, 0, 900, 870)]);
        assert_eq!(m.get_storage(pa, U256::from(0u64)), Some(U256::from(870u64)));
        assert_eq!(m.get_storage(pb, U256::from(0u64)), Some(U256::from(650u64)));
        // A canonical fetcher agreeing with the composed post-state → reconcile finds no drift.
        let canon = MapFetcher::new().with(pa, 0, 870).with(pb, 0, 650);
        let r = j.reconcile_block(&m, 300, &canon, ReconcileMode::Logs, 64);
        assert_eq!(r.mismatched, 0, "full ordered application is head-consistent");
    }

    #[test]
    fn evicting_a_flagged_block_counts_as_unreconciled_gap() {
        let m = StateMirror::new();
        let pool = [0x0b; 20];
        let mut j = MirrorJournal::new(1); // ring of 1 → each new block evicts the prior
        j.record_and_apply(&m, 1, txh(1), &[cs(pool, 0, 0, 1)]);
        j.flag_reconcile(1); // block 1 degraded but never reconciled
        j.record_and_apply(&m, 2, txh(2), &[cs(pool, 0, 1, 2)]); // evicts block 1
        assert_eq!(
            j.evicted_unreconciled(),
            1,
            "a flagged block dropped before reconcile is a counted coverage gap"
        );
    }

    #[test]
    fn block_tx_hashes_lists_journaled_hashes_in_order() {
        let m = StateMirror::new();
        let pool = [0x0c; 20];
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 400, txh(7), &[cs(pool, 0, 0, 1)]);
        j.record_and_apply(&m, 400, txh(9), &[cs(pool, 1, 0, 1)]);
        assert_eq!(j.block_tx_hashes(400), vec![txh(7), txh(9)]);
        assert!(j.block_tx_hashes(999).is_empty());
    }

    // ── Stage 5 · journal persistence (default-off identity + warm resume) ─────────────────────

    #[test]
    fn default_journal_is_not_persistent() {
        // Flag-off identity: MirrorJournal::new never opens a file and is byte-identical to Stage 1.
        let j = MirrorJournal::new(8);
        assert!(!j.is_persistent(), "in-RAM journal opens no persistence");
    }

    #[test]
    fn persistence_off_persist_close_is_a_noop() {
        // [WP5 · RED 13, journal side] With persistence off, persist_close writes nothing and never
        // touches disk — even when handed provenance. (The advancer-side "no recovery RPC when off" is
        // structural: the startup recovery + provenance fetch are gated on `is_persistent()`/persist_path.)
        let m = StateMirror::new();
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 1, txh(1), &[cs([0xaa; 20], 0, 0, 1)]);
        j.persist_close(1, Some(&dummy_prov(1))); // no-op, no panic, no file created
        assert!(!j.is_persistent());
    }

    fn jp_tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mantis_mj_persist_{}_{}_{}.redb",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    const TEST_CHAIN: u64 = 42_161;

    fn dummy_prov(h: u8) -> crate::mirror_journal_persist::BlockProvenance {
        crate::mirror_journal_persist::BlockProvenance {
            block_hash: [h; 32],
            parent_hash: [h.wrapping_sub(1); 32],
            state_root: [0x5a; 32],
            close_complete: true,
            reconciled: true,
            certified: true,
        }
    }

    #[test]
    fn resume_load_is_not_activation() {
        // [WP5 · F3 · RED 1/2/3] A restart over a populated persist file must NOT install anything: the
        // fresh journal starts empty, so (2) a persisted (block, tx_hash) can NOT return
        // DuplicateSkipped, and (3) persisted entries can NOT drive rollback or set an active head. Disk
        // bytes have zero effect on idempotency/ordering/rollback until the recovery protocol runs.
        use crate::mirror_journal_persist::JournalPersist;
        let path = jp_tmp("noactivate");
        let pool = [0x0d; 20];
        // "Run 1": journal two blocks with persistence, persisting each at close (provenanced).
        {
            let m = StateMirror::new();
            let persist = JournalPersist::open(&path, 8, TEST_CHAIN).unwrap();
            let mut j = MirrorJournal::with_persistence(8, persist);
            assert!(j.is_persistent());
            j.record_and_apply(&m, 500, txh(1), &[cs(pool, 0, 0, 10)]);
            j.record_and_apply(&m, 500, txh(2), &[cs(pool, 1, 0, 20)]);
            j.persist_close(500, Some(&dummy_prov(0x10)));
            j.record_and_apply(&m, 501, txh(3), &[cs(pool, 2, 0, 30)]);
            j.persist_close(501, Some(&dummy_prov(0x11)));
        }
        // "Run 2" (restart): a fresh journal over the SAME file installs NOTHING.
        {
            let m = StateMirror::new();
            let persist = JournalPersist::open(&path, 8, TEST_CHAIN).unwrap();
            let mut j = MirrorJournal::with_persistence(8, persist);
            assert_eq!(j.head_block(), 0, "F3: no head installed from disk");
            assert_eq!(j.entry_count(), 0, "F3: no entries installed from disk");
            assert_eq!(j.block_count(), 0, "F3: no ring installed from disk");
            assert!(!j.contains(500, &txh(1)), "F3: no idempotency key installed from disk");
            // RED 3: on the fresh journal (head 0, empty ring) NO persisted entry can drive a rollback —
            // there is no installed history to exact-inverse unwind (disk entries never become inverses).
            assert_eq!(
                j.plan_rollback_to(500),
                RollbackTo::NoOp,
                "fresh journal has no ring ⇒ disk entries never drive rollback"
            );
            // RED 2: a persisted tx is NOT skipped as a duplicate — it applies fresh.
            assert_eq!(
                j.record_and_apply(&m, 501, txh(3), &[cs(pool, 2, 0, 30)]),
                ApplyResult::Applied,
                "an inactive persisted key must not DuplicateSkip a fresh mirror"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn persistent_eviction_deletes_block_from_disk() {
        use crate::mirror_journal_persist::{JournalPersist, ResumeCandidate};
        let path = jp_tmp("evict");
        let pool = [0x0e; 20];
        {
            let m = StateMirror::new();
            let persist = JournalPersist::open(&path, 2, TEST_CHAIN).unwrap(); // ring of 2
            let mut j = MirrorJournal::with_persistence(2, persist);
            for b in 1u64..=3 {
                j.record_and_apply(&m, b, txh(b as u8), &[cs(pool, 0, 0, b)]);
                j.persist_close(b, Some(&dummy_prov(b as u8)));
            }
            // Block 1 was evicted when block 3 opened → also deleted from disk.
        }
        {
            let persist = JournalPersist::open(&path, 8, TEST_CHAIN).unwrap();
            match persist.load_candidate().unwrap() {
                ResumeCandidate::Provenanced { blocks, .. } => {
                    let nums: Vec<u64> = blocks.iter().map(|b| b.block).collect();
                    assert!(!nums.contains(&1), "evicted block deleted from disk: {nums:?}");
                    assert!(nums.contains(&2) && nums.contains(&3));
                }
                other => panic!("expected Provenanced, got {other:?}"),
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    // ── B5 · journal-driven multi-block reorg unwind (rollback_to) ─────────────────────────────

    #[test]
    fn rollback_to_deep_regression_exact_unwinds_multiple_blocks() {
        // Apply blocks N-2, N-1, N moving a SHARED slot across all three (so newest-first ordering
        // is load-bearing) plus a unique slot per newer block. A regression to N-2 must exact-unwind
        // N and N-1 only — state == end-of-(N-2) — and keep N.
        let m = StateMirror::new();
        let p = [0xB5; 20]; // shared across all three blocks
        let q = [0xB6; 20]; // touched only in N-1
        let r = [0xB7; 20]; // touched only in N
        seed(&m, p, 0, 100); // start-of-(N-2)
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 500, txh(1), &[cs(p, 0, 100, 110)]); // N-2
        j.record_and_apply(&m, 501, txh(2), &[cs(p, 0, 110, 120), cs(q, 0, 0, 50)]); // N-1
        j.record_and_apply(&m, 502, txh(3), &[cs(p, 0, 120, 130), cs(r, 0, 0, 60)]); // N
        assert_eq!(m.get_storage(p, U256::from(0u64)), Some(U256::from(130u64)));
        assert_eq!(j.head_block(), 502);

        match j.rollback_to(&m, 500) {
            RollbackTo::Unwound { footprint, blocks_unwound } => {
                assert_eq!(blocks_unwound, 2, "exact-unwinds N and N-1 only");
                // Union footprint of the unwound range (N-1 ∪ N) = {p, q, r}.
                assert!(footprint.contains(&(p, U256::from(0u64))));
                assert!(footprint.contains(&(q, U256::from(0u64))));
                assert!(footprint.contains(&(r, U256::from(0u64))));
            }
            other => panic!("expected Unwound, got {other:?}"),
        }
        // Shared slot rewound past the intermediate (120) to the block's start-of-block value (110).
        assert_eq!(
            m.get_storage(p, U256::from(0u64)),
            Some(U256::from(110u64)),
            "shared slot restored to end-of-(N-2), not an intermediate"
        );
        assert_eq!(j.head_block(), 500, "head reset to the regressed-to block");
        // Unwound blocks pruned + idempotency keys purged.
        assert!(j.footprint(501).is_empty() && j.footprint(502).is_empty());
        assert!(!j.contains(501, &txh(2)) && !j.contains(502, &txh(3)));
        // The kept block (N-2) survives with its entry + key.
        assert!(j.contains(500, &txh(1)) && !j.footprint(500).is_empty());
        assert_eq!(j.reorg_unwinds(), 1);
        assert_eq!(j.reorg_blocks_unwound(), 2);
    }

    #[test]
    fn rollback_to_below_ring_is_beyond_journal_no_mutation() {
        let m = StateMirror::new();
        let p = [0xB8; 20];
        seed(&m, p, 0, 100);
        let mut j = MirrorJournal::new(2); // ring of 2 → the oldest block evicts
        j.record_and_apply(&m, 500, txh(1), &[cs(p, 0, 100, 110)]);
        j.record_and_apply(&m, 501, txh(2), &[cs(p, 0, 110, 120)]);
        j.record_and_apply(&m, 502, txh(3), &[cs(p, 0, 120, 130)]); // evicts 500 → ring {501,502}
        assert_eq!(j.oldest_block(), 501);
        // target 500 is below the oldest retained block (501) → cannot exact-unwind.
        assert_eq!(j.rollback_to(&m, 500), RollbackTo::BeyondJournal);
        // NO mutation performed — mirror + journal head untouched (the caller degrades + resyncs).
        assert_eq!(m.get_storage(p, U256::from(0u64)), Some(U256::from(130u64)));
        assert_eq!(j.head_block(), 502);
        assert_eq!(j.block_count(), 2);
        assert_eq!(j.reorg_beyond_journal(), 1);
        assert_eq!(j.reorg_unwinds(), 0);
    }

    #[test]
    fn rollback_to_target_at_or_above_head_is_noop() {
        let m = StateMirror::new();
        let p = [0xB9; 20];
        seed(&m, p, 0, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 500, txh(1), &[cs(p, 0, 1, 2)]);
        j.record_and_apply(&m, 501, txh(2), &[cs(p, 0, 2, 3)]);
        assert_eq!(j.rollback_to(&m, 501), RollbackTo::NoOp, "target == head → nothing newer");
        assert_eq!(j.rollback_to(&m, 999), RollbackTo::NoOp, "target > head → nothing newer");
        assert_eq!(m.get_storage(p, U256::from(0u64)), Some(U256::from(3u64)), "no-op left mirror as-is");
        assert_eq!(j.head_block(), 501);
        assert_eq!(j.reorg_unwinds(), 0);
    }

    #[test]
    fn single_block_regression_unwinds_one_block_via_the_mechanism() {
        // The METHOD can unwind a single block too; the ADVANCER policy of routing depth-1 to the
        // existing per-block path is tested in shadow_revm.rs (classify_feed_advance). Here we prove
        // rollback_to's one-block case is the value-exact inverse of a single applied block.
        let m = StateMirror::new();
        let p = [0xB9; 20];
        seed(&m, p, 0, 10);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 700, txh(1), &[cs(p, 0, 10, 20)]);
        j.record_and_apply(&m, 701, txh(2), &[cs(p, 0, 20, 30)]);
        match j.rollback_to(&m, 700) {
            RollbackTo::Unwound { blocks_unwound, .. } => assert_eq!(blocks_unwound, 1),
            other => panic!("expected Unwound, got {other:?}"),
        }
        assert_eq!(m.get_storage(p, U256::from(0u64)), Some(U256::from(20u64)), "end-of-700");
        assert_eq!(j.head_block(), 700);
    }

    #[test]
    fn rollback_to_then_reapply_forward_reopens_blocks() {
        // After a deep unwind the advancer re-applies the reorged blocks forward; the journal must
        // ACCEPT them (open fresh blocks), not reject as out-of-order or dedup them.
        let m = StateMirror::new();
        let p = [0xBA; 20];
        seed(&m, p, 0, 100);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 500, txh(1), &[cs(p, 0, 100, 110)]);
        j.record_and_apply(&m, 501, txh(2), &[cs(p, 0, 110, 120)]);
        j.record_and_apply(&m, 502, txh(3), &[cs(p, 0, 120, 130)]);
        assert!(matches!(j.rollback_to(&m, 500), RollbackTo::Unwound { .. }));
        assert_eq!(j.head_block(), 500);
        // Re-deliver a REORGED 501' (distinct hash + effect) — must be Applied, not rejected/deduped.
        assert_eq!(
            j.record_and_apply(&m, 501, txh(0x51), &[cs(p, 0, 110, 200)]),
            ApplyResult::Applied,
            "reorged block re-applies forward after the unwind"
        );
        assert_eq!(m.get_storage(p, U256::from(0u64)), Some(U256::from(200u64)));
        assert_eq!(j.head_block(), 501);
    }

    #[test]
    fn rollback_to_unwinding_flagged_block_is_not_a_coverage_gap() {
        // A degraded-flagged block that is ACTIVELY unwound (repaired) is distinct from one silently
        // evicted before reconcile — it must NOT bump evicted_unreconciled.
        let m = StateMirror::new();
        let p = [0xBF; 20];
        seed(&m, p, 0, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 500, txh(1), &[cs(p, 0, 1, 2)]);
        j.record_and_apply(&m, 501, txh(2), &[cs(p, 0, 2, 3)]);
        j.flag_reconcile(501);
        assert!(matches!(j.rollback_to(&m, 500), RollbackTo::Unwound { .. }));
        assert_eq!(
            j.evicted_unreconciled(),
            0,
            "an actively unwound flagged block is repaired, not a dropped coverage gap"
        );
    }

    // ── B5 · post-unwind footprint reconcile against canonical ────────────────────────────────

    #[test]
    fn reconcile_footprint_repairs_divergent_slots() {
        let m = StateMirror::new();
        let p = [0xBB; 20];
        let q = [0xBC; 20];
        // Mirror holds a rolled-back state that drifted from canonical on slot p (a prior gap).
        m.apply_batch([
            (p, U256::from(0u64), U256::from(110u64)),
            (q, U256::from(0u64), U256::from(7u64)),
        ]);
        let footprint = vec![(p, U256::from(0u64)), (q, U256::from(0u64))];
        // Canonical-at-target: p=999 (drift), q=7 (agrees).
        let canon = MapFetcher::new().with(p, 0, 999).with(q, 0, 7);
        let rep = reconcile_footprint(&m, &footprint, &canon);
        assert_eq!(rep.checked, 2);
        assert_eq!(rep.mismatched, 1);
        assert_eq!(rep.repaired, 1);
        assert!(!rep.forced);
        assert_eq!(
            m.get_storage(p, U256::from(0u64)),
            Some(U256::from(999u64)),
            "diverged slot repaired to canonical"
        );
        assert_eq!(
            m.get_storage(q, U256::from(0u64)),
            Some(U256::from(7u64)),
            "agreeing slot untouched"
        );
    }

    #[test]
    fn reconcile_footprint_clean_no_repair() {
        let m = StateMirror::new();
        let p = [0xBD; 20];
        m.apply_batch([(p, U256::from(0u64), U256::from(42u64))]);
        let rep = reconcile_footprint(
            &m,
            &[(p, U256::from(0u64))],
            &MapFetcher::new().with(p, 0, 42),
        );
        assert_eq!(rep, ReconcileReport { checked: 1, mismatched: 0, repaired: 0, forced: false });
    }

    // ── B5 · full-resync reset (beyond-journal degrade support) ───────────────────────────────

    #[test]
    fn reset_clears_ring_for_full_resync() {
        let m = StateMirror::new();
        let p = [0xBE; 20];
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 500, txh(1), &[cs(p, 0, 0, 1)]);
        j.record_and_apply(&m, 501, txh(2), &[cs(p, 0, 1, 2)]);
        j.reset();
        assert_eq!(j.head_block(), 0);
        assert_eq!(j.block_count(), 0);
        assert!(!j.contains(500, &txh(1)));
        // After a reset the journal accepts a fresh forward stream from ANY block (the resync head).
        assert_eq!(
            j.record_and_apply(&m, 400, txh(9), &[cs(p, 0, 2, 3)]),
            ApplyResult::Applied,
            "reset journal re-opens from the resync block"
        );
        assert_eq!(j.head_block(), 400);
    }

    // ── [WP1] checked, transactional reconcile (fail-closed) ─────────────────────────────────────

    /// A CHECKED canonical fetcher: serves proven values from a map, but returns a typed
    /// [`engine_revm::CanonicalFetchError`] for any slot in `fail` — the fail-closed seam that
    /// `reconcile_block_checked` consumes (a tampered proof, unavailable header/proof, or untrusted
    /// anchor all surface here as an `Err`). Its infallible `fetch_storage` is intentionally NOT the
    /// trust path.
    struct CheckedMapFetcher {
        slots: ahash::AHashMap<([u8; 20], U256), U256>,
        fail: ahash::AHashSet<([u8; 20], U256)>,
        fail_kind: CanonicalFetchErrorKind,
    }
    impl CheckedMapFetcher {
        fn new() -> Self {
            Self {
                slots: ahash::AHashMap::new(),
                fail: ahash::AHashSet::new(),
                fail_kind: CanonicalFetchErrorKind::InvalidStorageProof,
            }
        }
        fn with(mut self, addr: [u8; 20], slot: u64, val: u64) -> Self {
            self.slots.insert((addr, U256::from(slot)), U256::from(val));
            self
        }
        fn failing(mut self, addr: [u8; 20], slot: u64) -> Self {
            self.fail.insert((addr, U256::from(slot)));
            self
        }
        fn fail_kind(mut self, kind: CanonicalFetchErrorKind) -> Self {
            self.fail_kind = kind;
            self
        }
    }
    impl StateFetcher for CheckedMapFetcher {
        fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (U256::ZERO, 0, None)
        }
        fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
            self.slots.get(&(a, slot)).copied().unwrap_or(U256::ZERO)
        }
        fn fetch_storage_checked(
            &self,
            a: [u8; 20],
            slot: U256,
        ) -> Result<U256, engine_revm::CanonicalFetchError> {
            if self.fail.contains(&(a, slot)) {
                return Err(engine_revm::CanonicalFetchError::new(
                    self.fail_kind,
                    0,
                    a,
                    slot,
                    "test forced canonical failure",
                ));
            }
            Ok(self.slots.get(&(a, slot)).copied().unwrap_or(U256::ZERO))
        }
    }

    #[test]
    fn checked_fetch_failure_is_fail_closed_no_mutation() {
        // WP1 RED-1/RED-2/RED-3: any canonical failure (tampered proof, unavailable header/proof,
        // untrusted anchor — all surface as fetch_storage_checked Err) yields NO value to reconcile,
        // performs ZERO mirror mutation, and records debt. A failed fetch is NEVER a mismatch.
        let m = StateMirror::new();
        let pool = [0x30; 20];
        seed(&m, pool, 0, 7);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(pool, 0, 7, 7)]);
        let before = m.get_storage(pool, U256::from(0u64));
        for kind in [
            CanonicalFetchErrorKind::UntrustedAnchor,
            CanonicalFetchErrorKind::HeaderUnavailable,
            CanonicalFetchErrorKind::ProofUnavailable,
            CanonicalFetchErrorKind::InvalidStorageProof,
        ] {
            let canon = CheckedMapFetcher::new().failing(pool, 0).fail_kind(kind);
            let out = j.reconcile_block_checked(&m, 42, &canon, ReconcileMode::Logs, 64, None);
            assert!(!out.complete, "{kind:?}: a canonical failure is incomplete");
            assert_eq!(out.canonical_failures, 1, "{kind:?}");
            assert_eq!(out.mismatched, 0, "{kind:?}: a failed fetch is never a mismatch");
            assert!(!out.publishable, "{kind:?}");
            assert!(!out.unresolved.is_empty(), "{kind:?}: debt records the footprint");
            assert_eq!(out.last_error, Some(kind), "{kind:?}: typed reason surfaced");
            assert!(!j.debt().is_empty(), "{kind:?}");
            assert_eq!(
                m.get_storage(pool, U256::from(0u64)),
                before,
                "{kind:?}: mirror byte-identical after a failed pass"
            );
        }
    }

    #[test]
    fn checked_one_failed_key_applies_zero_repairs() {
        // WP1 RED-4: one failed key in a multi-key repair applies ZERO keys (repair is all-or-none,
        // staged BEFORE any write). sample_n=1 audits only s0 (which diverges → repair triggered);
        // s2 then fails during repair staging ⇒ nothing is written.
        let m = StateMirror::new();
        let a = [0x31; 20];
        seed(&m, a, 0, 1);
        seed(&m, a, 1, 1);
        seed(&m, a, 2, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(
            &m,
            42,
            txh(1),
            &[cs(a, 0, 1, 999), cs(a, 1, 1, 888), cs(a, 2, 1, 777)],
        );
        // s0 fetchable (mismatches mirror → triggers repair), s1 fetchable, s2 FAILS in repair staging.
        let canon = CheckedMapFetcher::new().with(a, 0, 10).with(a, 1, 20).failing(a, 2);
        let out = j.reconcile_block_checked(&m, 42, &canon, ReconcileMode::Logs, 1, None);
        assert!(!out.complete, "a failed repair key ⇒ incomplete");
        assert_eq!(out.repaired, 0, "one failed key ⇒ ZERO keys applied");
        assert_eq!(m.get_storage(a, U256::from(0u64)), Some(U256::from(999u64)), "s0 not partially repaired");
        assert_eq!(m.get_storage(a, U256::from(1u64)), Some(U256::from(888u64)), "s1 not partially repaired");
        assert_eq!(m.get_storage(a, U256::from(2u64)), Some(U256::from(777u64)), "s2 not partially repaired");
    }

    #[test]
    fn checked_incomplete_pass_does_not_clear_flag() {
        // WP1 RED-5/RED-6: a failed pass keeps the block unresolved (flag stays), sets debt, and is
        // not publishable.
        let m = StateMirror::new();
        let a = [0x32; 20];
        seed(&m, a, 0, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(a, 0, 1, 1)]);
        j.flag_reconcile(42);
        assert!(j.is_flagged(42));
        let canon = CheckedMapFetcher::new().failing(a, 0);
        let out = j.reconcile_block_checked(&m, 42, &canon, ReconcileMode::Sampled, 64, None);
        assert!(!out.complete && !out.publishable);
        assert!(out.forced, "the flagged block reconciles forced");
        assert!(j.is_flagged(42), "an incomplete pass must NOT clear the forced-reconcile flag");
    }

    #[test]
    fn checked_valid_repair_commits_all_then_clears_flag() {
        // WP1 RED-7: a valid all-key repair commits every canonical value, and only THEN clears the
        // flag + debt and becomes publishable.
        let m = StateMirror::new();
        let a = [0x33; 20];
        seed(&m, a, 0, 1);
        seed(&m, a, 1, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(a, 0, 1, 999), cs(a, 1, 1, 888)]);
        j.flag_reconcile(42);
        let canon = CheckedMapFetcher::new().with(a, 0, 10).with(a, 1, 20);
        let out = j.reconcile_block_checked(&m, 42, &canon, ReconcileMode::Logs, 64, None);
        assert!(out.complete && out.publishable);
        assert!(out.repaired >= 2);
        assert_eq!(m.get_storage(a, U256::from(0u64)), Some(U256::from(10u64)));
        assert_eq!(m.get_storage(a, U256::from(1u64)), Some(U256::from(20u64)));
        assert!(!j.is_flagged(42), "flag cleared only after a complete repair");
        assert!(j.debt().is_empty());
    }

    #[test]
    fn checked_unrelated_clean_block_cannot_heal_prior_debt() {
        // WP1 RED-8: block N fails (debt on x). A later block whose OWN footprint is clean but which
        // still cannot re-prove x does NOT heal — the prior debt persists and it is not publishable.
        let m = StateMirror::new();
        let x = [0x34; 20];
        let y = [0x35; 20];
        seed(&m, x, 0, 5);
        seed(&m, y, 0, 5);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(x, 0, 5, 5)]);
        let o1 = j.reconcile_block_checked(&m, 42, &CheckedMapFetcher::new().failing(x, 0), ReconcileMode::Logs, 64, None);
        assert!(!o1.complete && !j.debt().is_empty());
        // Block 43: y is clean, but x STILL fails.
        j.record_and_apply(&m, 43, txh(2), &[cs(y, 0, 5, 5)]);
        let canon = CheckedMapFetcher::new().with(y, 0, 5).failing(x, 0);
        let o2 = j.reconcile_block_checked(&m, 43, &canon, ReconcileMode::Logs, 64, None);
        assert!(!o2.complete, "a clean block cannot heal while prior debt is unresolved");
        assert!(!o2.publishable);
        assert!(
            j.debt().slots().contains(&(x, U256::from(0u64))),
            "prior debt persists across an unrelated clean block"
        );
    }

    #[test]
    fn checked_reproof_at_newer_block_clears_debt_and_heals() {
        // WP1 RED-9: explicitly re-proving the carried footprint at a newer closed block clears the
        // debt and lets the mirror heal (publishable).
        let m = StateMirror::new();
        let x = [0x34; 20];
        let y = [0x35; 20];
        seed(&m, x, 0, 5);
        seed(&m, y, 0, 5);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(x, 0, 5, 5)]);
        j.reconcile_block_checked(&m, 42, &CheckedMapFetcher::new().failing(x, 0), ReconcileMode::Logs, 64, None);
        assert!(!j.debt().is_empty());
        // Block 44: x is now re-provable (and matches), y clean.
        j.record_and_apply(&m, 44, txh(3), &[cs(y, 0, 5, 5)]);
        let canon = CheckedMapFetcher::new().with(x, 0, 5).with(y, 0, 5);
        let o = j.reconcile_block_checked(&m, 44, &canon, ReconcileMode::Logs, 64, None);
        assert!(o.complete && o.publishable, "re-proof discharges debt ⇒ publishable");
        assert!(j.debt().is_empty(), "re-proving the carried footprint clears the debt");
    }

    #[test]
    fn checked_debt_over_age_cap_escalates_full_resync() {
        // WP1 RED-10: undischarged debt that exceeds its age cap escalates to a full resync — never a
        // silent eviction.
        let m = StateMirror::new();
        let x = [0x36; 20];
        seed(&m, x, 0, 5);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(x, 0, 5, 5)]);
        let fail = CheckedMapFetcher::new().failing(x, 0);
        let o1 = j.reconcile_block_checked(&m, 42, &fail, ReconcileMode::Logs, 64, None);
        assert!(!o1.full_resync_required, "fresh debt is within its age budget");
        assert_eq!(j.debt().since_block(), 42);
        let future = 42 + MAX_DEBT_AGE_BLOCKS + 1;
        let o2 = j.reconcile_block_checked(&m, future, &fail, ReconcileMode::Logs, 64, None);
        assert!(o2.full_resync_required, "aged, undischarged debt escalates to full resync");
        assert!(!j.debt().is_empty(), "debt is NOT silently evicted");
    }

    #[test]
    fn full_resync_latch_is_sticky_survives_reset_and_clean_reconcile() {
        // WP1 FIX-1: debt-cap escalation sets a STICKY full-resync latch that a plain clean close can
        // NEVER clear (the debt-laundering fix). It survives journal.reset() (reset is not the resync)
        // and a subsequent complete/clean reconcile; only explicit resync tooling clears it.
        let m = StateMirror::new();
        let x = [0x40; 20];
        seed(&m, x, 0, 5);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(x, 0, 5, 5)]);
        let fail = CheckedMapFetcher::new().failing(x, 0);
        j.reconcile_block_checked(&m, 42, &fail, ReconcileMode::Logs, 64, None); // opens debt @42
        let future = 42 + MAX_DEBT_AGE_BLOCKS + 1;
        let o = j.reconcile_block_checked(&m, future, &fail, ReconcileMode::Logs, 64, None);
        assert!(o.full_resync_required);
        assert!(j.full_resync_pending(), "escalation sets the sticky latch");
        // reset() clears debt + ring but NOT the latch.
        j.reset();
        assert!(j.debt().is_empty(), "reset clears debt");
        assert!(j.full_resync_pending(), "the latch survives reset() — reset is not the resync");
        // A fresh, fully clean reconcile does not clear the latch.
        let y = [0x41; 20];
        seed(&m, y, 0, 9);
        j.record_and_apply(&m, future + 1, txh(2), &[cs(y, 0, 9, 9)]);
        let o2 =
            j.reconcile_block_checked(&m, future + 1, &CheckedMapFetcher::new().with(y, 0, 9), ReconcileMode::Logs, 64, None);
        assert!(o2.complete, "the fresh block itself reconciles clean");
        assert!(j.full_resync_pending(), "a clean reconcile does NOT clear the sticky latch");
        // Only explicit resync tooling clears it.
        j.clear_full_resync_pending();
        assert!(!j.full_resync_pending());
    }

    #[test]
    fn debt_over_size_cap_escalates_full_resync() {
        // WP1 FIX-4b: the SIZE cap (not just the age cap) escalates. A single failing block whose
        // footprint exceeds MAX_DEBT_SLOTS opens over-cap debt ⇒ full_resync_required + sticky latch.
        let m = StateMirror::new();
        let pool = [0x45; 20];
        let mut slots = Vec::with_capacity(MAX_DEBT_SLOTS + 1);
        let mut fail = CheckedMapFetcher::new();
        for i in 0..(MAX_DEBT_SLOTS as u64 + 1) {
            seed(&m, pool, i, 1);
            slots.push(cs(pool, i, 1, 1));
            fail = fail.failing(pool, i);
        }
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &slots);
        let o = j.reconcile_block_checked(&m, 42, &fail, ReconcileMode::Logs, MAX_DEBT_SLOTS + 8, None);
        assert!(o.full_resync_required, "debt over the SIZE cap escalates to full resync");
        assert!(j.full_resync_pending(), "size-cap escalation sets the sticky latch");
        assert!(j.debt().len() > MAX_DEBT_SLOTS, "debt is not silently evicted");
    }

    #[test]
    fn checked_passthrough_fetcher_repairs_like_legacy() {
        // WP1: with a plain fetcher (default fetch_storage_checked = Ok(fetch_storage) — the
        // MIRROR_COLD_VERIFY=0 compatibility wrapper), the checked core behaves like the legacy
        // reconcile_block: a divergent slot is repaired to canonical and the block is publishable.
        let m = StateMirror::new();
        let a = [0x37; 20];
        seed(&m, a, 0, 1);
        let mut j = MirrorJournal::new(8);
        j.record_and_apply(&m, 42, txh(1), &[cs(a, 0, 1, 999)]);
        let canon = MapFetcher::new().with(a, 0, 42);
        let out = j.reconcile_block_checked(&m, 42, &canon, ReconcileMode::Logs, 64, None);
        assert!(out.complete && out.publishable);
        assert!(out.repaired >= 1);
        assert_eq!(m.get_storage(a, U256::from(0u64)), Some(U256::from(42u64)));
    }

    // ── [WP4 · Fix E] checked deep-reorg recovery: plan / verify / commit ────────────────────────

    #[test]
    fn plan_rollback_to_is_pure_and_checked_footprint_is_fail_closed() {
        // WP4 RED-1/2/3 (journal core): plan_rollback_to is PURE (no journal/mirror mutation) and the
        // checked footprint reconcile is all-or-none — one unverifiable slot ⇒ ZERO mirror mutation.
        let m = StateMirror::new();
        let a = [0x60; 20];
        seed(&m, a, 0, 1);
        seed(&m, a, 1, 1);
        let mut j = MirrorJournal::new(16);
        j.record_and_apply(&m, 100, txh(1), &[cs(a, 0, 1, 10)]); // target
        j.record_and_apply(&m, 101, txh(2), &[cs(a, 0, 10, 20)]);
        j.record_and_apply(&m, 102, txh(3), &[cs(a, 1, 1, 30)]);
        let (head_before, blocks_before) = (j.head_block(), j.block_count());

        match j.plan_rollback_to(100) {
            RollbackTo::Unwound { footprint, blocks_unwound } => {
                assert_eq!(blocks_unwound, 2);
                assert!(footprint.contains(&(a, U256::from(0u64))));
                assert!(footprint.contains(&(a, U256::from(1u64))));
            }
            other => panic!("expected Unwound, got {other:?}"),
        }
        // PURE: plan mutated neither the journal nor the mirror.
        assert_eq!(j.head_block(), head_before);
        assert_eq!(j.block_count(), blocks_before);
        assert_eq!(m.get_storage(a, U256::from(0u64)), Some(U256::from(20u64)));

        // Checked footprint reconcile: a/1 fails to verify ⇒ fail-closed, NO mutation.
        let footprint = vec![(a, U256::from(0u64)), (a, U256::from(1u64))];
        let (b0, b1) = (m.get_storage(a, U256::from(0u64)), m.get_storage(a, U256::from(1u64)));
        let fail = CheckedMapFetcher::new().with(a, 0, 5).failing(a, 1);
        let out = reconcile_footprint_checked(&m, &footprint, &fail, None);
        assert!(!out.complete, "one unverifiable slot ⇒ incomplete");
        assert_eq!(m.get_storage(a, U256::from(0u64)), b0, "no mutation on a failed checked reorg reconcile");
        assert_eq!(m.get_storage(a, U256::from(1u64)), b1, "no mutation on a failed checked reorg reconcile");

        // Valid full-footprint proof ⇒ applies the canonical-at-target patch (the rewind).
        let ok = CheckedMapFetcher::new().with(a, 0, 10).with(a, 1, 1);
        let out2 = reconcile_footprint_checked(&m, &footprint, &ok, None);
        assert!(out2.complete);
        assert_eq!(m.get_storage(a, U256::from(0u64)), Some(U256::from(10u64)), "footprint rewound to canonical-at-target");
        assert_eq!(m.get_storage(a, U256::from(1u64)), Some(U256::from(1u64)));

        // COMMIT the journal prune (only after every value verified): blocks > target gone, head=target.
        j.commit_rollback_prune(100, 2);
        assert_eq!(j.head_block(), 100, "prune resets head to target");
        assert_eq!(j.block_count(), 1, "blocks above target pruned");
    }

    #[test]
    fn plan_rollback_to_beyond_journal_and_noop_are_pure() {
        // WP4 RED-4 (component): beyond-journal + no-op classifications are pure (no ledger mutation).
        let m = StateMirror::new();
        let a = [0x61; 20];
        let mut j = MirrorJournal::new(4);
        j.record_and_apply(&m, 200, txh(1), &[cs(a, 0, 0, 1)]);
        j.record_and_apply(&m, 201, txh(2), &[cs(a, 0, 1, 2)]);
        assert_eq!(j.plan_rollback_to(100), RollbackTo::BeyondJournal, "below oldest ⇒ beyond journal");
        assert_eq!(j.reorg_beyond_journal(), 0, "plan is pure — no ledger increment");
        j.note_reorg_beyond_journal();
        assert_eq!(j.reorg_beyond_journal(), 1, "the caller books the beyond-journal ledger");
        assert_eq!(j.plan_rollback_to(201), RollbackTo::NoOp, "target == head ⇒ nothing newer");
        assert_eq!(j.plan_rollback_to(999), RollbackTo::NoOp, "target > head ⇒ nothing newer");
    }

    // ── WP5 · F4 canonical resume recovery ─────────────────────────────────────────────────────

    fn pb(
        block: u64,
        hash: u8,
        parent: u8,
        slots: &[([u8; 20], u64, u64)],
    ) -> crate::mirror_journal_persist::PersistedBlock {
        let entries = vec![JournalEntry {
            tx_hash: [block as u8; 32],
            slots: slots
                .iter()
                .map(|(a, s, v)| ChangedSlot {
                    address: *a,
                    slot: U256::from(*s),
                    original: U256::ZERO,
                    present: U256::from(*v),
                })
                .collect(),
        }];
        crate::mirror_journal_persist::PersistedBlock {
            block,
            block_hash: [hash; 32],
            parent_hash: [parent; 32],
            state_root: [0x5a; 32],
            close_complete: true,
            reconciled: true,
            certified: true,
            entries,
        }
    }

    #[test]
    fn recover_and_validate_happy_path_then_activate_installs_warm() {
        // WP5 · F4 · RED 4/5 (+ FIX-AUDIT-1(a)/3): a contiguous canonical prefix validates and hydrates
        // its footprint at the anchor; activate installs it (hydrated + retagged + head set +
        // RESUME_PENDING cleared) but HOLDS it untrusted — the resume footprint is folded into debt and
        // the (possibly-reorged) anchor snapshot is NOT published until the first close re-proves it.
        // A clean re-prove (canon == hydrated) then clears the debt and lets the mirror heal. Fresh
        // journal + fresh mirror before activation.
        let pool = [0xA0; 20];
        let blocks = vec![
            pb(100, 0x10, 0x0f, &[(pool, 0, 111)]),
            pb(101, 0x11, 0x10, &[(pool, 1, 222)]),
        ];
        // canonical headers agree with the persisted hashes; live head is at/above the top block.
        let header = |n: u64| -> Result<[u8; 32], ResumeError> {
            match n {
                100 => Ok([0x10; 32]),
                101 => Ok([0x11; 32]),
                _ => Err(ResumeError::HeaderUnavailable(n)),
            }
        };
        // canonical values at the anchor for the footprint {pool/0, pool/1}.
        let fetcher = |_anchor: u64| CheckedMapFetcher::new().with(pool, 0, 111).with(pool, 1, 222);
        let activated =
            recover_and_validate(&blocks, 101, header, fetcher, None).expect("valid resume");
        assert_eq!(activated.anchor_block, 101);
        assert_eq!(activated.prefix_blocks, 2);
        assert_eq!(activated.patch.len(), 2);

        let mirror = StateMirror::new();
        mirror.set_reason(crate::state_mirror::health_reason::RESUME_PENDING);
        let mut j = MirrorJournal::new(8);
        j.activate(&mirror, activated);
        assert_eq!(mirror.get_storage(pool, U256::from(0u64)), Some(U256::from(111u64)), "hydrated");
        assert_eq!(mirror.get_storage(pool, U256::from(1u64)), Some(U256::from(222u64)));
        assert_eq!(mirror.block(), 101, "retagged to the anchor");
        assert_eq!(j.head_block(), 101, "journal head set to the anchor");
        assert!(!mirror.has_reason(crate::state_mirror::health_reason::RESUME_PENDING), "RESUME_PENDING cleared");
        // FIX-AUDIT-1(a)/3: untrusted (resume footprint in debt) + NOT published until the first re-prove.
        assert!(!mirror.is_healthy(), "the un-re-proved anchor is held untrusted (PROOF_UNRESOLVED)");
        assert!(mirror.published().is_none(), "the possibly-reorged anchor is not published at activate");
        assert!(!j.debt().is_empty(), "the resume footprint is folded into debt for re-proof");

        // First close re-proves the resume footprint at the anchor; canon matches the hydrated values ⇒
        // the debt clears and the mirror can heal (a real advancer close then publishes).
        let canon = CheckedMapFetcher::new().with(pool, 0, 111).with(pool, 1, 222);
        let out = j.reconcile_block_checked(&mirror, 101, &canon, ReconcileMode::Logs, 64, None);
        assert!(out.complete && out.publishable, "clean re-prove of the resume footprint is publishable");
        assert!(j.debt().is_empty(), "debt cleared after a clean re-proof");
    }

    #[test]
    fn recover_and_validate_drops_orphan_top_and_finds_common_ancestor() {
        // WP5 · F4 · RED 7: a restart-spanning reorg orphaned the persisted TOP block (its hash no longer
        // matches canonical). Recovery must drop it and anchor at the retained common ancestor below.
        let pool = [0xA1; 20];
        let blocks = vec![
            pb(200, 0x20, 0x1f, &[(pool, 0, 500)]),
            pb(201, 0x21, 0x20, &[(pool, 1, 600)]), // orphaned during downtime
        ];
        let header = |n: u64| -> Result<[u8; 32], ResumeError> {
            match n {
                200 => Ok([0x20; 32]),       // still canonical
                201 => Ok([0xEE; 32]),       // canonical hash differs ⇒ persisted 201 is an orphan
                _ => Err(ResumeError::HeaderUnavailable(n)),
            }
        };
        let fetcher = |_a: u64| CheckedMapFetcher::new().with(pool, 0, 500);
        let activated =
            recover_and_validate(&blocks, 201, header, fetcher, None).expect("anchors at ancestor");
        assert_eq!(activated.anchor_block, 200, "orphan top dropped; anchor at the common ancestor");
        assert_eq!(activated.prefix_blocks, 1);
    }

    #[test]
    fn recover_and_validate_all_orphaned_is_no_common_ancestor() {
        // WP5 · F4 · RED 7: no retained block matches canonical ⇒ discard + full resync.
        let pool = [0xA2; 20];
        let blocks = vec![pb(300, 0x30, 0x2f, &[(pool, 0, 1)])];
        let header = |_n: u64| -> Result<[u8; 32], ResumeError> { Ok([0xEE; 32]) };
        let fetcher = |_a: u64| CheckedMapFetcher::new();
        assert_eq!(
            recover_and_validate(&blocks, 300, header, fetcher, None).unwrap_err(),
            ResumeError::NoCommonAncestor
        );
    }

    #[test]
    fn recover_and_validate_persisted_head_above_live_fails_safe() {
        // WP5 · F4 · RED 8: every retained block sits above the live head ⇒ none can be canonical yet.
        let pool = [0xA3; 20];
        let blocks = vec![pb(900, 0x90, 0x8f, &[(pool, 0, 1)])];
        let header = |_n: u64| -> Result<[u8; 32], ResumeError> { Ok([0x90; 32]) };
        let fetcher = |_a: u64| CheckedMapFetcher::new().with(pool, 0, 1);
        // live head is far below the persisted block ⇒ skipped ⇒ no ancestor.
        assert_eq!(
            recover_and_validate(&blocks, 800, header, fetcher, None).unwrap_err(),
            ResumeError::NoCommonAncestor
        );
    }

    #[test]
    fn recover_and_validate_broken_parent_link_discards() {
        // WP5 · F4: the accepted prefix must chain (parent links + consecutive numbers). A broken link
        // ⇒ discard (never trust a non-contiguous prefix).
        let pool = [0xA4; 20];
        let blocks = vec![
            pb(400, 0x40, 0x3f, &[(pool, 0, 1)]),
            pb(401, 0x41, 0xCC, &[(pool, 1, 2)]), // parent_hash 0xCC != block 400's hash 0x40
        ];
        let header = |n: u64| -> Result<[u8; 32], ResumeError> {
            match n {
                400 => Ok([0x40; 32]),
                401 => Ok([0x41; 32]),
                _ => Err(ResumeError::HeaderUnavailable(n)),
            }
        };
        let fetcher = |_a: u64| CheckedMapFetcher::new().with(pool, 0, 1).with(pool, 1, 2);
        assert_eq!(
            recover_and_validate(&blocks, 401, header, fetcher, None).unwrap_err(),
            ResumeError::BrokenChain(401)
        );
    }

    #[test]
    fn recover_and_validate_hydration_is_all_or_none() {
        // WP5 · F4 · RED 6: one unprovable footprint slot ⇒ HydrationIncomplete and ZERO patch (no
        // partial activation). Validation of chain identity passed; the checked hydration is the gate.
        let pool = [0xA5; 20];
        let blocks = vec![pb(500, 0x50, 0x4f, &[(pool, 0, 10), (pool, 1, 20)])];
        let header = |n: u64| -> Result<[u8; 32], ResumeError> {
            if n == 500 { Ok([0x50; 32]) } else { Err(ResumeError::HeaderUnavailable(n)) }
        };
        // pool/1 cannot be proven at the anchor ⇒ the whole hydration fails.
        let fetcher = |_a: u64| CheckedMapFetcher::new().with(pool, 0, 10).failing(pool, 1);
        assert_eq!(
            recover_and_validate(&blocks, 500, header, fetcher, None).unwrap_err(),
            ResumeError::HydrationIncomplete
        );
    }

    #[test]
    fn recover_full_resync_clears_latch_only_after_full_reproof() {
        // WP5 · WP4 carry-over: an in-run full-resync escalation clears FULL_RESYNC_REQUIRED ONLY after
        // the WHOLE resident footprint re-proves at a current checked anchor. A single unprovable slot
        // keeps the mirror latched (untrusted) — no restart-only dead end, and no premature re-trust.
        let mirror = StateMirror::new();
        let a = [0xB0; 20];
        seed(&mirror, a, 0, 111);
        seed(&mirror, a, 1, 222);
        let mut j = MirrorJournal::new(8);
        j.set_full_resync_pending();

        // One slot cannot be proven ⇒ recovery incomplete ⇒ latch STAYS.
        let failing = CheckedMapFetcher::new().with(a, 0, 111).failing(a, 1);
        assert!(!j.recover_full_resync(&mirror, &failing, None), "incomplete re-proof ⇒ no recovery");
        assert!(j.full_resync_pending(), "latch stays set until the whole footprint re-proves");

        // All slots prove ⇒ recovery completes ⇒ latch clears (first in-process clear).
        let ok = CheckedMapFetcher::new().with(a, 0, 111).with(a, 1, 222);
        assert!(j.recover_full_resync(&mirror, &ok, None), "full re-proof recovers");
        assert!(!j.full_resync_pending(), "latch cleared only after the complete re-proof");
    }

    #[test]
    fn resume_anchor_block_is_audited_not_served_stale() {
        // FIX-AUDIT-1 (reproduced fail-open): after activate() sets head=anchor with an EMPTY ring, a tx
        // AT the anchor block was silently DROPPED (empty ring ⇒ block_mut None) yet still applied to the
        // mirror, and the resume footprint was never journaled — so reconcile_block_checked(anchor)
        // audited nothing (checked=0), returned complete+publishable, and a same-number anchor reorg
        // served the STALE hydrated value. FIX (a) folds the resume footprint into debt (re-proved at the
        // first close) + (c) journals the anchor tx instead of dropping it → the divergence is DETECTED
        // and repaired. RED before fix: checked=0 / complete / publishable / stale value served.
        let mirror = StateMirror::new();
        let pool = [0xD9; 20];
        let mut j = MirrorJournal::new(8);
        // Warm resume: hydrate pool/0 = 999 (as if the anchor was reorged away), activate at anchor 101.
        j.activate(
            &mirror,
            ActivatedResume {
                anchor_block: 101,
                patch: vec![(pool, U256::from(0u64), Proven::checked(U256::from(999u64)))],
                prefix_blocks: 1,
            },
        );
        // FIX-3 + FIX-1(a): the possibly-reorged anchor is held UNTRUSTED (resume footprint in debt) and
        // NOT published until the first close re-proves it.
        assert!(!mirror.is_healthy(), "activate holds the anchor untrusted (debt) until re-proved");
        assert!(mirror.published().is_none(), "the un-re-proved anchor snapshot is NOT published");

        // A tx arrives AT the anchor block (same number) after resume — must be journaled, not dropped.
        assert_eq!(
            j.record_and_apply(&mirror, 101, txh(9), &[cs(pool, 1, 0, 888)]),
            ApplyResult::Applied
        );

        // Close the anchor block: canonical truth diverges from the resume-hydrated + applied values.
        let canon = CheckedMapFetcher::new().with(pool, 0, 111).with(pool, 1, 222);
        let out = j.reconcile_block_checked(&mirror, 101, &canon, ReconcileMode::Logs, 64, None);
        assert!(out.checked >= 1, "the resume footprint is AUDITED at the anchor close (not checked=0)");
        assert!(out.mismatched >= 1, "the anchor divergence is DETECTED");
        assert_eq!(
            mirror.get_storage(pool, U256::from(0u64)),
            Some(U256::from(111u64)),
            "the stale resume value (999) is repaired to canonical (111), never served"
        );
        assert_eq!(
            mirror.get_storage(pool, U256::from(1u64)),
            Some(U256::from(222u64)),
            "the anchor-block tx value (888) is likewise re-proved to canonical (222)"
        );
    }

    #[test]
    fn recover_full_resync_also_clears_reorg_recovery() {
        // FIX-AUDIT-2 (liveness): a complete in-run full-resync re-proves the WHOLE resident footprint,
        // which SUBSUMES the reorg footprint — so it must clear BOTH the sticky FULL_RESYNC_REQUIRED AND
        // REORG_RECOVERY, letting a reorg-degraded mirror recover in-run instead of staying latched until
        // another reorg/restart. An INCOMPLETE re-prove clears neither.
        use crate::state_mirror::health_reason;
        let mirror = StateMirror::new();
        let a = [0xBA; 20];
        seed(&mirror, a, 0, 1);
        let mut j = MirrorJournal::new(8);
        // Simulate a reorg-failure latch: both REORG_RECOVERY and the sticky full-resync latch.
        mirror.set_reason(health_reason::REORG_RECOVERY);
        mirror.set_reason(health_reason::FULL_RESYNC_REQUIRED);
        j.set_full_resync_pending();

        // Incomplete re-prove ⇒ neither reason cleared (still untrusted).
        let failing = CheckedMapFetcher::new().failing(a, 0);
        assert!(!j.recover_full_resync(&mirror, &failing, None));
        assert!(mirror.has_reason(health_reason::REORG_RECOVERY), "incomplete ⇒ REORG_RECOVERY stays");
        assert!(mirror.has_reason(health_reason::FULL_RESYNC_REQUIRED), "incomplete ⇒ latch stays");

        // Complete re-prove ⇒ BOTH cleared ⇒ mirror healthy in-run.
        let ok = CheckedMapFetcher::new().with(a, 0, 1);
        assert!(j.recover_full_resync(&mirror, &ok, None));
        assert!(!mirror.has_reason(health_reason::REORG_RECOVERY), "complete re-prove clears REORG_RECOVERY");
        assert!(!mirror.has_reason(health_reason::FULL_RESYNC_REQUIRED), "complete re-prove clears the latch");
        assert!(mirror.is_healthy(), "a reorg-degraded mirror recovers in-run after a complete re-prove");
    }
}
