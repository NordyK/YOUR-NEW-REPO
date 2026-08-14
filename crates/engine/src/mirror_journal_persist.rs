//! [Stage 5 · Journal persistence] Optional disk backing for the Stage-1 [`MirrorJournal`] ring so a
//! restart can *warm-resume* — but ONLY after the persisted records are proven to belong to the
//! canonical chain. Backed by **redb** (pure-Rust, ACID, copy-on-write B-trees; default
//! `Durability::Immediate` 1PC+C, so a committed write is fsynced and a crash rolls back to the last
//! complete commit — never a partial record).
//!
//! # Fix F: persisted resume is a validation protocol, not a byte round-trip (WP5)
//!
//! The pre-WP5 store round-tripped `(block, tx_hash)` keys + `Vec<ChangedSlot>` and installed them as
//! trusted resume state on load (Finding 5). It stored NO provenance (block hash/parent/state root,
//! chain id, close/reconcile status), silently skipped corrupt records, and trusted encoded counts for
//! `Vec::with_capacity`. This module replaces that with:
//!
//! * **F1 — versioned provenance schema.** Every record carries magic + schema version + chain id +
//!   block number/hash/parent-hash/state-root + close/reconcile/certify status + a content checksum,
//!   written in ONE redb transaction per completed block. A separate META row carries the journal-wide
//!   status (full-resync/debt latches + head). Old-schema data (no META) is quarantined, never trusted
//!   by version upgrade alone.
//! * **F2 — strict, bounded decode.** [`JournalPersist::load_candidate`] returns a strict
//!   [`ResumeCandidate`], never a silently-truncated set. Before any allocation it enforces max record
//!   bytes, max entries/slots (per record and aggregate), checked arithmetic, known magic/version/chain
//!   id, exact end-of-input (no trailing bytes), and ordered/unique blocks. One invalid record makes
//!   the whole candidate `Unusable` (discard/quarantine), never a partial "warm" resume.
//! * **F3 — load is not activation.** `load_candidate` only DECODES; it installs nothing. The resume
//!   candidate stays inactive (mirror `RESUME_PENDING`) until the canonical-recovery protocol
//!   (`recover_and_validate` → `activate`, Fix F4) proves chain identity and hydrates a fresh mirror at
//!   one checked anchor. See [`crate::mirror_journal`] and [`crate::shadow_revm`].
//!
//! Default OFF (`MIRROR_JOURNAL_PERSIST=0`): [`MirrorJournal`] holds `persist: None`, opens no file,
//! and performs no load/decode/recovery — byte-identical to Stage 1.

use std::path::Path;

use alloy_primitives::U256;
use engine_revm::ChangedSlot;
use redb::{Database, ReadableTable, TableDefinition};

use crate::mirror_journal::JournalEntry;

/// redb table (schema v2): block number → provenanced, checksummed block record.
const BLOCKS_V2: TableDefinition<u64, &[u8]> = TableDefinition::new("mirror_journal_blocks_v2");
/// redb table: single-row journal-wide status/provenance header (key [`META_KEY`]).
const META: TableDefinition<u64, &[u8]> = TableDefinition::new("mirror_journal_meta_v2");
/// The pre-WP5 (unprovenanced) table name — detected on load so a legacy file is explicitly
/// quarantined rather than silently ignored (F1: never trusted by version upgrade alone).
const LEGACY_BLOCKS: TableDefinition<u64, &[u8]> = TableDefinition::new("mirror_journal_blocks");

const META_KEY: u64 = 0;

/// Test-only checkpoints around the one application-level block+META transaction. Production builds
/// contain neither the enum nor the phase hook; the default and enabled persistence paths are
/// unchanged outside `cfg(test)`.
#[cfg(all(test, unix))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistCrashPhase {
    AfterBlockInsert,
    AfterMetaInsert,
    AfterCommit,
}

/// Per-record magic (`"MJB2"`) — the leading bytes of every block record.
const BLOCK_MAGIC: [u8; 4] = *b"MJB2";
/// Per-record magic (`"MJM2"`) — the leading bytes of the META record.
const META_MAGIC: [u8; 4] = *b"MJM2";
/// Schema version. Bump on any wire change; a decoded record whose version != this is quarantined.
const SCHEMA_VERSION: u16 = 2;

// ── Resource caps (enforced BEFORE allocation; F2 / RED test 10) ─────────────────────────────────
/// Max bytes for one block record (generous vs a real block's journaled set, but bounded).
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
/// Max journaled entries (feed txs) in one block.
const MAX_ENTRIES_PER_BLOCK: usize = 200_000;
/// Max changed slots one entry may carry.
const MAX_SLOTS_PER_ENTRY: usize = 200_000;
/// Max changed slots aggregated across one block record.
const MAX_TOTAL_SLOTS_PER_BLOCK: usize = 2_000_000;
/// [FIX-C] Aggregate caps across ALL records in one load — bound total decode work so an adversarial
/// on-disk file cannot accumulate unbounded memory across records before the newest-`cap_blocks` drain
/// (the per-record caps alone don't bound the record COUNT). A legitimate file holds ≤ cap_blocks
/// records (evicted blocks are deleted), so these are generous ceilings, not tight fits.
const MAX_LOAD_RECORDS: usize = 100_000;
/// [FIX-C] Aggregate raw on-disk bytes decoded across all records in one load.
const MAX_LOAD_BYTES: usize = 512 * 1024 * 1024;
/// On-wire size of one encoded slot: addr(20) + slot(32) + original(32) + present(32).
const SLOT_BYTES: usize = 20 + 32 + 32 + 32;
/// On-wire size of one entry header: tx_hash(32) + n_slots(u32).
const ENTRY_HEADER_BYTES: usize = 32 + 4;
/// Fixed prefix of a block record before the entry list (magic..n_entries).
const BLOCK_HEADER_BYTES: usize = 4 + 2 + 8 + 8 + 32 + 32 + 32 + 1 + 4;

/// A persistence failure (redb / codec), carried as a reason string so the caller can log + degrade
/// (persistence is best-effort warmth, never a correctness dependency).
#[derive(Debug, Clone)]
pub struct PersistError(String);

impl PersistError {
    fn new(msg: impl Into<String>) -> Self {
        PersistError(msg.into())
    }
    /// The failure reason.
    pub fn reason(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for PersistError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PersistError {}

/// [F1] Canonical provenance + close status of a block, supplied by the advancer at persist time. A
/// block is persisted for warm-resume ONLY when its canonical identity is known (the checked path
/// resolves it); without provenance nothing trustworthy can be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockProvenance {
    /// The canonical block hash the advancer observed for this block.
    pub block_hash: [u8; 32],
    /// Its parent hash (for restart-spanning-reorg ancestor checks).
    pub parent_hash: [u8; 32],
    /// Its state root (defense-in-depth provenance; not required for the pragmatic recovery).
    pub state_root: [u8; 32],
    /// Whether the block closed completely (not a gap/abort).
    pub close_complete: bool,
    /// Whether its reconcile completed (publishable).
    pub reconciled: bool,
    /// Whether a certified snapshot was published for it.
    pub certified: bool,
}

/// [F1] A strictly-decoded persisted block record: its provenance + journaled entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedBlock {
    /// Block number (the redb key, cross-checked against the record body).
    pub block: u64,
    /// Canonical block hash as persisted.
    pub block_hash: [u8; 32],
    /// Parent hash as persisted.
    pub parent_hash: [u8; 32],
    /// State root as persisted.
    pub state_root: [u8; 32],
    /// Close completed.
    pub close_complete: bool,
    /// Reconcile completed.
    pub reconciled: bool,
    /// Certified snapshot published.
    pub certified: bool,
    /// The block's journaled entries (idempotency keys + changed slots).
    pub entries: Vec<JournalEntry>,
}

/// [F1] The journal-wide status persisted in the META row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalStatus {
    /// The journal head at last persist.
    pub head: u64,
    /// Sticky full-resync latch was set at shutdown (mirror was untrusted ⇒ candidate must discard).
    pub full_resync_pending: bool,
    /// Reconciliation debt was outstanding at shutdown (mirror was untrusted ⇒ candidate must discard).
    pub debt_present: bool,
}

/// [F3] The result of a strict `load_candidate`. Loaded records are INACTIVE — nothing is installed
/// until the canonical-recovery protocol validates and activates them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeCandidate {
    /// No usable persisted state (fresh/never-written db, or persistence just enabled). Cold start.
    Empty,
    /// The persisted state cannot be safely resumed and must be discarded/quarantined + cold/full
    /// resync: strict-decode failure, unknown/legacy schema, wrong chain id, or an untrusted-at-shutdown
    /// latch (full-resync/debt). Carries a human reason for logs/metrics.
    Unusable(String),
    /// A strictly-decoded, provenance-bearing candidate — still INACTIVE and untrusted until
    /// `recover_and_validate` proves it canonical.
    Provenanced {
        /// Chain id the records were written on (already checked == expected).
        chain_id: u64,
        /// Persisted journal head.
        head: u64,
        /// The retained blocks, ascending, deduplicated, ≤ `cap_blocks`.
        blocks: Vec<PersistedBlock>,
    },
}

/// redb-backed store for the mirror-journal ring. Bounded to `cap_blocks` on load (newest wins).
/// Provenanced schema v2 — see the module docs.
pub struct JournalPersist {
    db: Database,
    cap_blocks: usize,
    chain_id: u64,
}

impl JournalPersist {
    /// Open (or create) the journal database at `path`, retaining at most `cap_blocks` blocks on load
    /// and validating persisted records against `chain_id`. Creates the parent directory if needed.
    pub fn open(path: &Path, cap_blocks: usize, chain_id: u64) -> Result<Self, PersistError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PersistError::new(format!("create journal dir: {e}")))?;
            }
        }
        let db =
            Database::create(path).map_err(|e| PersistError::new(format!("open journal db: {e}")))?;
        Ok(Self {
            db,
            cap_blocks: cap_blocks.max(1),
            chain_id,
        })
    }

    /// [F1] Persist one block's provenanced record AND update the journal-wide META status in ONE redb
    /// transaction (crash-atomic: a partial write is never visible; the last committed block is the
    /// last complete one). Overwrites any prior value for that block.
    pub fn put_block(
        &self,
        block: u64,
        entries: &[JournalEntry],
        prov: &BlockProvenance,
        status: &JournalStatus,
    ) -> Result<(), PersistError> {
        #[cfg(all(test, unix))]
        {
            let mut no_phase_hook = |_| {};
            return self.put_block_inner(block, entries, prov, status, &mut no_phase_hook);
        }
        #[cfg(not(all(test, unix)))]
        {
            self.put_block_inner(block, entries, prov, status)
        }
    }

    fn put_block_inner(
        &self,
        block: u64,
        entries: &[JournalEntry],
        prov: &BlockProvenance,
        status: &JournalStatus,
        #[cfg(all(test, unix))] phase_hook: &mut dyn FnMut(PersistCrashPhase),
    ) -> Result<(), PersistError> {
        let rec = encode_block(self.chain_id, block, entries, prov);
        let meta = encode_meta(self.chain_id, status);
        let w = self
            .db
            .begin_write()
            .map_err(|e| PersistError::new(format!("begin_write: {e}")))?;
        {
            let mut t = w
                .open_table(BLOCKS_V2)
                .map_err(|e| PersistError::new(format!("open_table: {e}")))?;
            t.insert(&block, rec.as_slice())
                .map_err(|e| PersistError::new(format!("insert: {e}")))?;
            #[cfg(all(test, unix))]
            phase_hook(PersistCrashPhase::AfterBlockInsert);
            let mut m = w
                .open_table(META)
                .map_err(|e| PersistError::new(format!("open_meta: {e}")))?;
            m.insert(&META_KEY, meta.as_slice())
                .map_err(|e| PersistError::new(format!("insert_meta: {e}")))?;
            #[cfg(all(test, unix))]
            phase_hook(PersistCrashPhase::AfterMetaInsert);
        }
        w.commit()
            .map_err(|e| PersistError::new(format!("commit: {e}")))?;
        #[cfg(all(test, unix))]
        phase_hook(PersistCrashPhase::AfterCommit);
        Ok(())
    }

    /// Invoke the real `put_block` transaction with a test-only phase observer. A subprocess parks
    /// inside the observer and its parent sends SIGKILL, so pre-commit recovery does not rely on Rust
    /// unwinding or `WriteTransaction::drop`.
    #[cfg(all(test, unix))]
    fn put_block_with_phase_hook(
        &self,
        block: u64,
        entries: &[JournalEntry],
        prov: &BlockProvenance,
        status: &JournalStatus,
        phase_hook: &mut dyn FnMut(PersistCrashPhase),
    ) -> Result<(), PersistError> {
        self.put_block_inner(block, entries, prov, status, phase_hook)
    }

    /// [F1] Update only the journal-wide status (e.g. when a latch flips without a new block). One txn.
    pub fn put_status(&self, status: &JournalStatus) -> Result<(), PersistError> {
        let meta = encode_meta(self.chain_id, status);
        let w = self
            .db
            .begin_write()
            .map_err(|e| PersistError::new(format!("begin_write: {e}")))?;
        {
            let mut m = w
                .open_table(META)
                .map_err(|e| PersistError::new(format!("open_meta: {e}")))?;
            m.insert(&META_KEY, meta.as_slice())
                .map_err(|e| PersistError::new(format!("insert_meta: {e}")))?;
        }
        w.commit()
            .map_err(|e| PersistError::new(format!("commit: {e}")))?;
        Ok(())
    }

    /// Delete a block from disk (called when the in-RAM ring evicts it — keeps disk bounded).
    pub fn delete_block(&self, block: u64) -> Result<(), PersistError> {
        let w = self
            .db
            .begin_write()
            .map_err(|e| PersistError::new(format!("begin_write: {e}")))?;
        {
            let mut t = w
                .open_table(BLOCKS_V2)
                .map_err(|e| PersistError::new(format!("open_table: {e}")))?;
            t.remove(&block)
                .map_err(|e| PersistError::new(format!("remove: {e}")))?;
        }
        w.commit()
            .map_err(|e| PersistError::new(format!("commit: {e}")))?;
        Ok(())
    }

    /// [F2/F3] Strictly decode all persisted state into an INACTIVE [`ResumeCandidate`]. Never installs
    /// anything and never silently drops corruption: any strict-decode failure, unknown/legacy schema,
    /// wrong chain id, or an untrusted-at-shutdown latch makes the whole candidate `Unusable`.
    pub fn load_candidate(&self) -> Result<ResumeCandidate, PersistError> {
        let r = self
            .db
            .begin_read()
            .map_err(|e| PersistError::new(format!("begin_read: {e}")))?;

        // META presence decides fresh-vs-legacy-vs-provenanced. A missing META row but a present LEGACY
        // table = a pre-WP5 file → quarantine (never trust by upgrade alone). No tables at all = fresh.
        let meta_bytes = match r.open_table(META) {
            Ok(t) => match t.get(&META_KEY) {
                Ok(Some(v)) => v.value().to_vec(),
                Ok(None) | Err(_) => {
                    return Ok(self.empty_or_legacy(&r));
                }
            },
            Err(_) => {
                return Ok(self.empty_or_legacy(&r));
            }
        };
        let status = match decode_meta(&meta_bytes, self.chain_id) {
            Ok(s) => s,
            Err(e) => return Ok(ResumeCandidate::Unusable(format!("meta decode: {e}"))),
        };
        // A mirror that was untrusted at shutdown (sticky full-resync or outstanding debt) cannot be
        // warm-resumed — its journaled slots were never proven. Discard the whole candidate.
        if status.full_resync_pending {
            return Ok(ResumeCandidate::Unusable(
                "full-resync latch set at shutdown".into(),
            ));
        }
        if status.debt_present {
            return Ok(ResumeCandidate::Unusable(
                "reconciliation debt outstanding at shutdown".into(),
            ));
        }

        let t = match r.open_table(BLOCKS_V2) {
            Ok(t) => t,
            // META present but no block table ⇒ nothing to resume (treat as empty warm set).
            Err(_) => {
                return Ok(ResumeCandidate::Provenanced {
                    chain_id: self.chain_id,
                    head: status.head,
                    blocks: Vec::new(),
                })
            }
        };
        let iter = t
            .iter()
            .map_err(|e| PersistError::new(format!("iter: {e}")))?;
        let mut blocks: Vec<PersistedBlock> = Vec::new();
        let mut total_records = 0usize;
        let mut total_bytes = 0usize;
        for item in iter {
            let (k, v) = item.map_err(|e| PersistError::new(format!("iter item: {e}")))?;
            let raw = v.value();
            // [FIX-C] Enforce the AGGREGATE caps BEFORE decoding this record — so an adversarial file
            // with an unbounded number of (or unbounded total-bytes across) records cannot accumulate
            // before the ring-cap drain. One breach ⇒ the whole candidate is Unusable (discard).
            total_records += 1;
            total_bytes = total_bytes.saturating_add(raw.len());
            if !aggregate_within_caps(total_records, total_bytes) {
                return Ok(ResumeCandidate::Unusable(format!(
                    "aggregate decode cap exceeded ({total_records} records / {total_bytes} bytes)"
                )));
            }
            let key = k.value();
            match decode_block(raw, self.chain_id) {
                Ok(pb) => {
                    if pb.block != key {
                        return Ok(ResumeCandidate::Unusable(format!(
                            "record body block {} != redb key {key}",
                            pb.block
                        )));
                    }
                    blocks.push(pb);
                }
                // F2: ONE invalid retained record ⇒ the whole candidate is incomplete. Discard.
                Err(e) => {
                    return Ok(ResumeCandidate::Unusable(format!(
                        "block {key} record decode: {e}"
                    )))
                }
            }
        }
        blocks.sort_by_key(|b| b.block);
        // Ordered + unique (the redb key set is unique, but guard the decoded view too).
        for w in blocks.windows(2) {
            if w[0].block == w[1].block {
                return Ok(ResumeCandidate::Unusable(format!(
                    "duplicate block {}",
                    w[0].block
                )));
            }
        }
        if blocks.len() > self.cap_blocks {
            let drop = blocks.len() - self.cap_blocks;
            blocks.drain(0..drop);
        }
        Ok(ResumeCandidate::Provenanced {
            chain_id: self.chain_id,
            head: status.head,
            blocks,
        })
    }

    /// Classify a db with no META row. A present legacy table ⇒ quarantine. [FIX-D] v2 block records
    /// WITHOUT a META row ⇒ a torn/partial write (the one-txn `put_block` commits block+META together,
    /// so a clean crash never leaves this — redb 1PC+C rolls back a partial commit); treat it as
    /// Unusable rather than silently ignoring orphan block records. Otherwise the db is genuinely empty.
    fn empty_or_legacy(&self, r: &redb::ReadTransaction) -> ResumeCandidate {
        if r.open_table(LEGACY_BLOCKS).is_ok() {
            return ResumeCandidate::Unusable(
                "legacy pre-provenance schema (no META) — quarantined".into(),
            );
        }
        if let Ok(t) = r.open_table(BLOCKS_V2) {
            let has_any = match t.iter() {
                Ok(mut it) => it.next().is_some(),
                Err(_) => true, // can't read the table ⇒ treat as suspect, not empty
            };
            if has_any {
                return ResumeCandidate::Unusable(
                    "block records present without a META row — torn/corrupt write, discarded".into(),
                );
            }
        }
        ResumeCandidate::Empty
    }
}

// ── Compact codec (little-endian header, big-endian U256; strict + bounded decode) ───────────────

/// [FIX-C] Whether a running (records, bytes) total across a load is still within the aggregate caps.
fn aggregate_within_caps(records: usize, bytes: usize) -> bool {
    records <= MAX_LOAD_RECORDS && bytes <= MAX_LOAD_BYTES
}

/// FNV-1a 32-bit digest over `bytes` — an integrity check against accidental/disk corruption (NOT an
/// adversarial guard: an attacker who can rewrite the file can rewrite the digest; canonical validation
/// at recovery is the adversarial defense).
fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// [F1] Encode a block's provenanced record. Layout (LE header, BE U256):
/// `MAGIC(4) ver(2) chain(8) block(8) block_hash(32) parent(32) state_root(32) status(1)
///  n_entries(4) [tx(32) n_slots(4) [addr(20) slot(32) orig(32) present(32)]...]... checksum(4)`.
fn encode_block(
    chain_id: u64,
    block: u64,
    entries: &[JournalEntry],
    prov: &BlockProvenance,
) -> Vec<u8> {
    let mut b = Vec::with_capacity(BLOCK_HEADER_BYTES + entries.len() * 64);
    b.extend_from_slice(&BLOCK_MAGIC);
    b.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    b.extend_from_slice(&chain_id.to_le_bytes());
    b.extend_from_slice(&block.to_le_bytes());
    b.extend_from_slice(&prov.block_hash);
    b.extend_from_slice(&prov.parent_hash);
    b.extend_from_slice(&prov.state_root);
    let status = (prov.close_complete as u8)
        | ((prov.reconciled as u8) << 1)
        | ((prov.certified as u8) << 2);
    b.push(status);
    b.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        b.extend_from_slice(&e.tx_hash);
        b.extend_from_slice(&(e.slots.len() as u32).to_le_bytes());
        for s in &e.slots {
            b.extend_from_slice(&s.address);
            b.extend_from_slice(&s.slot.to_be_bytes::<32>());
            b.extend_from_slice(&s.original.to_be_bytes::<32>());
            b.extend_from_slice(&s.present.to_be_bytes::<32>());
        }
    }
    let ck = fnv1a32(&b);
    b.extend_from_slice(&ck.to_le_bytes());
    b
}

/// A strict cursor over a record body: every read is bounds-checked; the caller verifies exact EOF.
struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cursor { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .p
            .checked_add(n)
            .ok_or_else(|| "length overflow".to_string())?;
        let s = self
            .b
            .get(self.p..end)
            .ok_or_else(|| format!("unexpected end of record at {}+{n}", self.p))?;
        self.p = end;
        Ok(s)
    }
    fn u16le(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32le(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64le(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn arr32(&mut self) -> Result<[u8; 32], String> {
        Ok(self.take(32)?.try_into().unwrap())
    }
}

/// [F2] Strict, bounded inverse of [`encode_block`]. Enforces (in order, BEFORE any per-count
/// allocation): max record bytes; magic; known version; chain id; per-entry/per-block/aggregate slot
/// caps with checked arithmetic that the remaining bytes can actually hold the claimed counts; a
/// verified checksum; and EXACT end-of-input (no trailing bytes).
fn decode_block(bytes: &[u8], expected_chain: u64) -> Result<PersistedBlock, String> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(format!("record {} bytes exceeds cap", bytes.len()));
    }
    if bytes.len() < BLOCK_HEADER_BYTES + 4 {
        return Err("record shorter than header+checksum".into());
    }
    // Verify the checksum over everything but the trailing 4-byte digest before trusting any field.
    let body = &bytes[..bytes.len() - 4];
    let want = u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap());
    if fnv1a32(body) != want {
        return Err("checksum mismatch".into());
    }
    let mut c = Cursor::new(body);
    if c.take(4)? != BLOCK_MAGIC {
        return Err("bad block magic".into());
    }
    let ver = c.u16le()?;
    if ver != SCHEMA_VERSION {
        return Err(format!("unknown schema version {ver}"));
    }
    let chain = c.u64le()?;
    if chain != expected_chain {
        return Err(format!("chain id {chain} != expected {expected_chain}"));
    }
    let block = c.u64le()?;
    let block_hash = c.arr32()?;
    let parent_hash = c.arr32()?;
    let state_root = c.arr32()?;
    let status = c.take(1)?[0];
    let n_entries = c.u32le()? as usize;
    if n_entries > MAX_ENTRIES_PER_BLOCK {
        return Err(format!("n_entries {n_entries} exceeds cap"));
    }
    // Bound the allocation by proving the remaining bytes can hold at least the entry headers.
    let remaining = body.len() - c.p;
    let min_needed = n_entries
        .checked_mul(ENTRY_HEADER_BYTES)
        .ok_or("entries*header overflow")?;
    if min_needed > remaining {
        return Err(format!(
            "declared {n_entries} entries need ≥{min_needed}B but {remaining}B remain"
        ));
    }
    let mut entries = Vec::with_capacity(n_entries);
    let mut total_slots = 0usize;
    for _ in 0..n_entries {
        let tx_hash: [u8; 32] = c.arr32()?;
        let n_slots = c.u32le()? as usize;
        if n_slots > MAX_SLOTS_PER_ENTRY {
            return Err(format!("n_slots {n_slots} exceeds cap"));
        }
        total_slots = total_slots
            .checked_add(n_slots)
            .ok_or("total slots overflow")?;
        if total_slots > MAX_TOTAL_SLOTS_PER_BLOCK {
            return Err(format!("aggregate slots {total_slots} exceeds cap"));
        }
        let need = n_slots.checked_mul(SLOT_BYTES).ok_or("slots*stride overflow")?;
        if need > body.len() - c.p {
            return Err(format!(
                "declared {n_slots} slots need {need}B but {}B remain",
                body.len() - c.p
            ));
        }
        let mut slots = Vec::with_capacity(n_slots);
        for _ in 0..n_slots {
            let address: [u8; 20] = c.take(20)?.try_into().unwrap();
            let slot = U256::from_be_slice(c.take(32)?);
            let original = U256::from_be_slice(c.take(32)?);
            let present = U256::from_be_slice(c.take(32)?);
            slots.push(ChangedSlot {
                address,
                slot,
                original,
                present,
            });
        }
        entries.push(JournalEntry { tx_hash, slots });
    }
    // Exact EOF: no trailing data after the last entry (before the checksum we already split off).
    if c.p != body.len() {
        return Err(format!("trailing {} bytes after entries", body.len() - c.p));
    }
    Ok(PersistedBlock {
        block,
        block_hash,
        parent_hash,
        state_root,
        close_complete: status & 1 != 0,
        reconciled: status & (1 << 1) != 0,
        certified: status & (1 << 2) != 0,
        entries,
    })
}

/// [F1] Encode the META row: `MAGIC(4) ver(2) chain(8) flags(1) head(8) checksum(4)`.
fn encode_meta(chain_id: u64, status: &JournalStatus) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + 2 + 8 + 1 + 8 + 4);
    b.extend_from_slice(&META_MAGIC);
    b.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    b.extend_from_slice(&chain_id.to_le_bytes());
    let flags = (status.full_resync_pending as u8) | ((status.debt_present as u8) << 1);
    b.push(flags);
    b.extend_from_slice(&status.head.to_le_bytes());
    let ck = fnv1a32(&b);
    b.extend_from_slice(&ck.to_le_bytes());
    b
}

/// [F2] Strict inverse of [`encode_meta`] (fixed size; checksum + magic + version + chain checked).
fn decode_meta(bytes: &[u8], expected_chain: u64) -> Result<JournalStatus, String> {
    const META_BYTES: usize = 4 + 2 + 8 + 1 + 8 + 4;
    if bytes.len() != META_BYTES {
        return Err(format!("meta size {} != {META_BYTES}", bytes.len()));
    }
    let body = &bytes[..bytes.len() - 4];
    let want = u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap());
    if fnv1a32(body) != want {
        return Err("meta checksum mismatch".into());
    }
    let mut c = Cursor::new(body);
    if c.take(4)? != META_MAGIC {
        return Err("bad meta magic".into());
    }
    let ver = c.u16le()?;
    if ver != SCHEMA_VERSION {
        return Err(format!("unknown meta version {ver}"));
    }
    let chain = c.u64le()?;
    if chain != expected_chain {
        return Err(format!("meta chain id {chain} != expected {expected_chain}"));
    }
    let flags = c.take(1)?[0];
    let head = c.u64le()?;
    Ok(JournalStatus {
        head,
        full_resync_pending: flags & 1 != 0,
        debt_present: flags & (1 << 1) != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAIN: u64 = 42_161;

    fn cs(addr: u8, slot: u64, orig: u64, present: u64) -> ChangedSlot {
        ChangedSlot {
            address: [addr; 20],
            slot: U256::from(slot),
            original: U256::from(orig),
            present: U256::from(present),
        }
    }

    fn entry(b: u8, slots: Vec<ChangedSlot>) -> JournalEntry {
        JournalEntry {
            tx_hash: [b; 32],
            slots,
        }
    }

    fn prov(h: u8, p: u8) -> BlockProvenance {
        BlockProvenance {
            block_hash: [h; 32],
            parent_hash: [p; 32],
            state_root: [0x5a; 32],
            close_complete: true,
            reconciled: true,
            certified: true,
        }
    }

    fn status(head: u64) -> JournalStatus {
        JournalStatus {
            head,
            full_resync_pending: false,
            debt_present: false,
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mantis_jp_{}_{}_{}.redb",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn block_record_roundtrips_with_provenance() {
        let entries = vec![
            entry(1, vec![cs(0xAA, 0, 100, 200), cs(0xBB, 5, 1, 2)]),
            entry(2, vec![]),
            entry(3, vec![cs(0xCC, 7, 9, 9)]),
        ];
        let enc = encode_block(CHAIN, 100, &entries, &prov(0x11, 0x22));
        let pb = decode_block(&enc, CHAIN).expect("decodes");
        assert_eq!(pb.block, 100);
        assert_eq!(pb.block_hash, [0x11; 32]);
        assert_eq!(pb.parent_hash, [0x22; 32]);
        assert!(pb.close_complete && pb.reconciled && pb.certified);
        assert_eq!(pb.entries.len(), 3);
        assert_eq!(pb.entries[0].slots.len(), 2);
        assert_eq!(pb.entries[0].slots[0].present, U256::from(200u64));
        assert_eq!(pb.entries[2].slots[0].slot, U256::from(7u64));
    }

    #[test]
    fn decode_rejects_truncation() {
        let enc = encode_block(CHAIN, 1, &[entry(1, vec![cs(0xAA, 0, 1, 2)])], &prov(1, 0));
        assert!(decode_block(&enc[..enc.len() - 3], CHAIN).is_err(), "truncation rejected");
    }

    #[test]
    fn decode_rejects_trailing_data() {
        let mut enc = encode_block(CHAIN, 1, &[entry(1, vec![])], &prov(1, 0));
        // Recompute a valid checksum over body+extra so ONLY the exact-EOF check can catch it.
        let extra = [0xEEu8; 3];
        let mut body = enc[..enc.len() - 4].to_vec();
        body.extend_from_slice(&extra);
        let ck = fnv1a32(&body);
        enc = body;
        enc.extend_from_slice(&ck.to_le_bytes());
        assert!(
            decode_block(&enc, CHAIN).is_err(),
            "trailing bytes (even with a valid checksum) must be rejected by the exact-EOF check"
        );
    }

    #[test]
    fn decode_rejects_bad_checksum() {
        let mut enc = encode_block(CHAIN, 1, &[entry(1, vec![cs(1, 1, 1, 1)])], &prov(1, 0));
        let n = enc.len();
        enc[n - 5] ^= 0xFF; // flip a body byte, leaving the stored checksum stale
        assert!(decode_block(&enc, CHAIN).is_err(), "checksum mismatch rejected");
    }

    #[test]
    fn decode_rejects_wrong_chain_and_version() {
        let enc = encode_block(CHAIN, 1, &[entry(1, vec![])], &prov(1, 0));
        assert!(decode_block(&enc, 1).is_err(), "wrong chain id rejected");
        // Corrupt the version field (bytes 4..6) and re-checksum so only the version check fires.
        let mut e = enc.clone();
        e[4] = 0xFF;
        let body_len = e.len() - 4;
        let ck = fnv1a32(&e[..body_len]);
        e[body_len..].copy_from_slice(&ck.to_le_bytes());
        assert!(decode_block(&e, CHAIN).is_err(), "unknown version rejected");
    }

    #[test]
    fn decode_rejects_attacker_entry_count_without_allocating() {
        // A record claiming a huge entry count but with no bytes to back it must be rejected by the
        // pre-allocation bound check, NOT by trying to allocate MAX_ENTRIES * capacity.
        let mut b = Vec::new();
        b.extend_from_slice(&BLOCK_MAGIC);
        b.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
        b.extend_from_slice(&CHAIN.to_le_bytes());
        b.extend_from_slice(&1u64.to_le_bytes());
        b.extend_from_slice(&[0u8; 32]);
        b.extend_from_slice(&[0u8; 32]);
        b.extend_from_slice(&[0u8; 32]);
        b.push(0);
        b.extend_from_slice(&100_000u32.to_le_bytes()); // claim 100k entries, provide none
        let ck = fnv1a32(&b);
        b.extend_from_slice(&ck.to_le_bytes());
        let err = decode_block(&b, CHAIN).unwrap_err();
        assert!(err.contains("entries need"), "bounded pre-alloc rejection: {err}");
    }

    #[test]
    fn decode_rejects_oversize_record() {
        let big = vec![0u8; MAX_RECORD_BYTES + 1];
        assert!(decode_block(&big, CHAIN).is_err(), "oversize record rejected pre-parse");
    }

    #[test]
    fn aggregate_caps_bound_total_decode_across_records() {
        // [FIX-C] load_candidate applies this per-record BEFORE decoding, so an adversarial file cannot
        // accumulate unbounded memory across records (the per-record caps don't bound the record count).
        assert!(aggregate_within_caps(1, 1_000));
        assert!(aggregate_within_caps(MAX_LOAD_RECORDS, MAX_LOAD_BYTES), "exactly at the caps is allowed");
        assert!(!aggregate_within_caps(MAX_LOAD_RECORDS + 1, 1_000), "too many records ⇒ rejected");
        assert!(!aggregate_within_caps(1, MAX_LOAD_BYTES + 1), "too many aggregate bytes ⇒ rejected");
    }

    #[test]
    fn meta_roundtrips_and_flags() {
        let s = JournalStatus {
            head: 12345,
            full_resync_pending: true,
            debt_present: false,
        };
        let dec = decode_meta(&encode_meta(CHAIN, &s), CHAIN).expect("decodes");
        assert_eq!(dec, s);
        assert!(decode_meta(&encode_meta(CHAIN, &s), 1).is_err(), "wrong chain rejected");
    }

    #[test]
    fn fresh_db_is_empty_candidate() {
        let path = tmp("fresh");
        let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
        assert_eq!(jp.load_candidate().unwrap(), ResumeCandidate::Empty);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn put_load_candidate_roundtrip_bounded() {
        let path = tmp("rt");
        {
            let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
            jp.put_block(100, &[entry(1, vec![cs(0xAA, 0, 100, 200)])], &prov(0x10, 0x0f), &status(100))
                .unwrap();
            jp.put_block(101, &[entry(2, vec![cs(0xBB, 1, 5, 6)]), entry(3, vec![])], &prov(0x11, 0x10), &status(101))
                .unwrap();
        }
        let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
        match jp.load_candidate().unwrap() {
            ResumeCandidate::Provenanced { chain_id, head, blocks } => {
                assert_eq!(chain_id, CHAIN);
                assert_eq!(head, 101);
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].block, 100);
                assert_eq!(blocks[0].block_hash, [0x10; 32]);
                assert_eq!(blocks[1].block, 101);
                assert_eq!(blocks[1].entries.len(), 2);
            }
            other => panic!("expected Provenanced, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_cap_keeps_newest() {
        let path = tmp("cap");
        {
            let jp = JournalPersist::open(&path, 2, CHAIN).unwrap();
            for b in 10u64..15 {
                jp.put_block(b, &[entry(b as u8, vec![])], &prov(b as u8, (b - 1) as u8), &status(b))
                    .unwrap();
            }
        }
        let jp = JournalPersist::open(&path, 2, CHAIN).unwrap();
        match jp.load_candidate().unwrap() {
            ResumeCandidate::Provenanced { blocks, .. } => {
                assert_eq!(blocks.len(), 2, "bounded to cap_blocks");
                assert_eq!(blocks[0].block, 13);
                assert_eq!(blocks[1].block, 14);
            }
            other => panic!("expected Provenanced, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn untrusted_at_shutdown_discards_whole_candidate() {
        let path = tmp("untrusted");
        {
            let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
            let mut s = status(50);
            s.full_resync_pending = true;
            jp.put_block(50, &[entry(1, vec![])], &prov(1, 0), &s).unwrap();
        }
        let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
        assert!(
            matches!(jp.load_candidate().unwrap(), ResumeCandidate::Unusable(_)),
            "a full-resync-latched shutdown discards the whole candidate"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wrong_chain_id_makes_candidate_unusable() {
        let path = tmp("wrongchain");
        {
            let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
            jp.put_block(1, &[entry(1, vec![])], &prov(1, 0), &status(1)).unwrap();
        }
        // Reopen expecting a DIFFERENT chain id — the persisted chain must not be trusted.
        let jp = JournalPersist::open(&path, 64, 1).unwrap();
        assert!(matches!(jp.load_candidate().unwrap(), ResumeCandidate::Unusable(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_schema_is_quarantined_not_trusted() {
        let path = tmp("legacy");
        // Simulate a pre-WP5 file: a LEGACY table present, no META row.
        {
            let db = Database::create(&path).unwrap();
            let w = db.begin_write().unwrap();
            {
                let mut t = w.open_table(LEGACY_BLOCKS).unwrap();
                t.insert(&7u64, [0u8; 8].as_slice()).unwrap();
            }
            w.commit().unwrap();
        }
        let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
        match jp.load_candidate().unwrap() {
            ResumeCandidate::Unusable(r) => assert!(r.contains("legacy"), "quarantined: {r}"),
            other => panic!("legacy schema must be quarantined, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_record_makes_whole_candidate_unusable() {
        let path = tmp("corrupt");
        {
            let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
            jp.put_block(1, &[entry(1, vec![])], &prov(1, 0), &status(2)).unwrap();
            jp.put_block(2, &[entry(2, vec![])], &prov(2, 1), &status(2)).unwrap();
        }
        // Corrupt block 2's record directly in redb (flip a body byte, leaving a stale checksum).
        {
            let db = Database::create(&path).unwrap();
            let cur = {
                let r = db.begin_read().unwrap();
                let t = r.open_table(BLOCKS_V2).unwrap();
                t.get(&2u64).unwrap().unwrap().value().to_vec()
            };
            let mut bad = cur.clone();
            bad[8] ^= 0xFF;
            let w = db.begin_write().unwrap();
            {
                let mut t = w.open_table(BLOCKS_V2).unwrap();
                t.insert(&2u64, bad.as_slice()).unwrap();
            }
            w.commit().unwrap();
        }
        let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
        assert!(
            matches!(jp.load_candidate().unwrap(), ResumeCandidate::Unusable(_)),
            "one corrupt record ⇒ the whole candidate is unusable (no silent partial resume)"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn block_without_meta_is_unusable_torn_write_proxy() {
        // [FIX-D] Torn-write proxy: `put_block` writes block+META in ONE redb txn (redb 1PC+C makes a
        // partial commit invisible — the last committed block is the last complete one). If a
        // block record is ever seen WITHOUT its META row (a truncated/corrupt file), load must treat the
        // candidate as Unusable, not silently ignore the orphan record. The SIGKILL test below
        // separately exercises real process death before and after the application-level commit;
        // this synthetic orphan covers the fail-closed response to otherwise impossible corruption.
        let path = tmp("tornwrite");
        {
            // Write a block record directly into BLOCKS_V2 with NO META row.
            let db = Database::create(&path).unwrap();
            let rec = encode_block(CHAIN, 42, &[entry(1, vec![])], &prov(42, 41));
            let w = db.begin_write().unwrap();
            {
                let mut t = w.open_table(BLOCKS_V2).unwrap();
                t.insert(&42u64, rec.as_slice()).unwrap();
            }
            w.commit().unwrap();
        }
        let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
        match jp.load_candidate().unwrap() {
            ResumeCandidate::Unusable(r) => assert!(r.contains("without a META"), "torn write: {r}"),
            other => panic!("a block without META must be Unusable, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    fn crash_phase_name(phase: PersistCrashPhase) -> &'static str {
        match phase {
            PersistCrashPhase::AfterBlockInsert => "after_block_insert",
            PersistCrashPhase::AfterMetaInsert => "after_meta_insert",
            PersistCrashPhase::AfterCommit => "after_commit",
        }
    }

    #[cfg(unix)]
    fn crash_phase_from_name(name: &str) -> PersistCrashPhase {
        match name {
            "after_block_insert" => PersistCrashPhase::AfterBlockInsert,
            "after_meta_insert" => PersistCrashPhase::AfterMetaInsert,
            "after_commit" => PersistCrashPhase::AfterCommit,
            other => panic!("unknown persistence crash phase {other}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn persistence_crash_child_entrypoint() {
        use std::io::Write as _;

        let Some(path) = std::env::var_os("MANTIS_JP_CRASH_DB") else {
            return;
        };
        let marker = std::path::PathBuf::from(
            std::env::var_os("MANTIS_JP_CRASH_MARKER").expect("crash marker path"),
        );
        let phase =
            crash_phase_from_name(&std::env::var("MANTIS_JP_CRASH_PHASE").expect("crash phase"));
        let jp = JournalPersist::open(std::path::Path::new(&path), 64, CHAIN).unwrap();
        let mut hook = |observed| {
            if observed == phase {
                let mut ready = std::fs::File::create(&marker).expect("create crash marker");
                ready.write_all(crash_phase_name(phase).as_bytes()).unwrap();
                ready.sync_all().expect("sync crash marker");
                loop {
                    std::thread::park();
                }
            }
        };
        jp.put_block_with_phase_hook(
            101,
            &[entry(2, vec![cs(0xBB, 1, 5, 6)])],
            &prov(0x11, 0x10),
            &status(101),
            &mut hook,
        )
        .expect("phase hook must stop the child before put_block returns");
        panic!("crash phase was never observed");
    }

    #[cfg(unix)]
    fn run_sigkill_phase(phase: PersistCrashPhase) -> ResumeCandidate {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let path = tmp(crash_phase_name(phase));
        let marker = path.with_extension("ready");
        {
            let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
            jp.put_block(
                100,
                &[entry(1, vec![cs(0xAA, 0, 100, 200)])],
                &prov(0x10, 0x0f),
                &status(100),
            )
            .unwrap();
        }

        let mut child = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "mirror_journal_persist::tests::persistence_crash_child_entrypoint",
            ])
            .env("MANTIS_JP_CRASH_DB", &path)
            .env("MANTIS_JP_CRASH_MARKER", &marker)
            .env("MANTIS_JP_CRASH_PHASE", crash_phase_name(phase))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn crash child");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            if let Some(status) = child.try_wait().expect("poll crash child") {
                panic!("crash child exited before phase marker: {status}");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("timed out waiting for {} marker", crash_phase_name(phase));
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // On Unix Child::kill is SIGKILL: no Rust destructors run, so redb must recover from an
        // actually interrupted process rather than from WriteTransaction::drop/abort.
        child.kill().expect("SIGKILL crash child");
        let killed = child.wait().expect("reap crash child");
        assert!(!killed.success(), "crash child must not exit successfully");

        let candidate = JournalPersist::open(&path, 64, CHAIN)
            .expect("redb reopens after SIGKILL")
            .load_candidate()
            .expect("strict candidate decode after SIGKILL");
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&path);
        candidate
    }

    #[cfg(unix)]
    #[test]
    fn persistence_sigkill_is_atomic_at_each_application_phase() {
        for phase in [
            PersistCrashPhase::AfterBlockInsert,
            PersistCrashPhase::AfterMetaInsert,
        ] {
            match run_sigkill_phase(phase) {
                ResumeCandidate::Provenanced {
                    chain_id,
                    head,
                    blocks,
                } => {
                    assert_eq!(chain_id, CHAIN);
                    assert_eq!(head, 100, "pre-commit SIGKILL must retain prior META");
                    assert_eq!(
                        blocks.iter().map(|b| b.block).collect::<Vec<_>>(),
                        vec![100],
                        "pre-commit SIGKILL must expose neither a partial block nor partial META"
                    );
                }
                other => panic!(
                    "pre-commit SIGKILL must recover the prior complete commit, got {other:?}"
                ),
            }
        }

        match run_sigkill_phase(PersistCrashPhase::AfterCommit) {
            ResumeCandidate::Provenanced {
                chain_id,
                head,
                blocks,
            } => {
                assert_eq!(chain_id, CHAIN);
                assert_eq!(head, 101, "post-commit SIGKILL must retain committed META");
                assert_eq!(
                    blocks.iter().map(|b| b.block).collect::<Vec<_>>(),
                    vec![100, 101],
                    "post-commit SIGKILL must expose the whole block+META commit"
                );
            }
            other => {
                panic!("post-commit SIGKILL must recover the new complete commit, got {other:?}")
            }
        }
    }

    #[test]
    fn delete_block_removes_from_disk() {
        let path = tmp("delete");
        let jp = JournalPersist::open(&path, 64, CHAIN).unwrap();
        jp.put_block(5, &[entry(1, vec![])], &prov(5, 4), &status(6)).unwrap();
        jp.put_block(6, &[entry(2, vec![])], &prov(6, 5), &status(6)).unwrap();
        jp.delete_block(5).unwrap();
        match jp.load_candidate().unwrap() {
            ResumeCandidate::Provenanced { blocks, .. } => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].block, 6);
            }
            other => panic!("expected Provenanced, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }
}
