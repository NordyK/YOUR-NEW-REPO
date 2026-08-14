//! T0.2 — local state mirror + read-through fetcher (the per-slot-RPC bottleneck fix).
//!
//! Mantis's REVM validator reads state through [`engine_revm::StateFetcher`]; the production
//! impl ([`crate::revm_fork::RpcStateFetcher`]) does a **synchronous RPC round-trip per account
//! (×3) and per storage slot** — ~30–80 ms each against a remote provider, which is the engine's
//! #1 validation-throughput wall. This module mirrors account + storage state in RAM and serves
//! reads from it, demoting RPC to a **cold, miss-only fallback** (a hit is ~ns). The mirror is
//! prewarmed at boot and kept live by the event stream (wiring TBD); this file is the
//! read-through core, copied in spirit from Tigger's three-tier `local_executor` Database
//! (`state_mirror/storage.rs` + `local_executor/database.rs`).
//!
//! Correctness: a storage-only cached account must NOT fabricate account data — `get_account`
//! returns `None` until the account's balance/nonce/code are actually primed, so a fetch still
//! falls back for them. Write-through caches whatever the fallback returned, including zeros.

use ahash::AHashMap;
use alloy_primitives::U256;
use arc_swap::ArcSwapOption;
use engine_revm::StateFetcher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Sentinel block meaning "the mirror holds no block-consistent state" — every read falls back.
pub const NO_BLOCK: u64 = 0;

/// Where a mirrored storage value came from — the freshness class it belongs to.
///
/// `Shadow` values are head-fresh by construction (post-state of an observed feed tx, published
/// by the shadow worker) and stay valid across validator repins. `Fallback` values were
/// write-through-cached from the PINNED-block RPC fetcher on a mirror miss — valid only for that
/// pin, so they are purged when the mirror is retagged to a different block (measured live
/// 2026-07-08: pin-block values surviving repins were a mirror-audit divergence source).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlotSource {
    Shadow,
    Fallback,
}

#[derive(Clone, Copy)]
struct SlotEntry {
    value: U256,
    source: SlotSource,
}

#[derive(Default, Clone)]
struct AccountEntry {
    /// Whether balance/nonce/code have been primed (distinguishes a real account from an entry
    /// that only carries a few cached storage slots).
    has_account_data: bool,
    balance: U256,
    nonce: u64,
    code: Option<Vec<u8>>,
    storage: AHashMap<U256, SlotEntry>,
}

/// [WP2 · Fix C] Blocking mirror-health reasons (a bitset in [`StateMirror::reasons`]). The mirror is
/// trusted ([`StateMirror::is_healthy`]) ONLY when NO reason is set. Each reason is OWNED by the
/// subsystem that can prove it resolved: a subsystem may set/clear ONLY its own reason, so clearing
/// one (e.g. a queue gap the advancer proved gone) can never erase another (e.g. an unresolved proof
/// debt). This replaces the single undifferentiated `unhealthy` bool whose blanket clear could
/// launder a proof/recovery trip.
pub mod health_reason {
    /// A lossless-queue gap: the block may be missing a tx never observed (advancer). Healable by a
    /// gapless checked close.
    pub const QUEUE_GAP: u32 = 1 << 0;
    /// Gross divergence: ≥ trip% of a checked sample diverged from canonical (advancer or repin audit).
    pub const GROSS_DIVERGENCE: u32 = 1 << 1;
    /// A canonical value could not be proven (proof/anchor/fetch failure) or reconciliation debt is
    /// outstanding (advancer). Clears ONLY when all debt is re-proven at one current anchor.
    pub const PROOF_UNRESOLVED: u32 = 1 << 2;
    /// A persisted-journal resume candidate is not yet validated/activated (WP5). Declared here; WP5
    /// owns setting/clearing it.
    pub const RESUME_PENDING: u32 = 1 << 3;
    /// A deep-reorg unwind reconcile is unproven / in recovery (advancer reorg path; WP4 tightens
    /// ownership to a checked reorg reconcile).
    pub const REORG_RECOVERY: u32 = 1 << 4;
    /// Reconciliation debt exceeded its size/age cap: the mirror needs an explicit full resync. Sticky
    /// — mirrors the journal's WP1 `full_resync_pending` latch 1:1; a plain clean close cannot clear it.
    pub const FULL_RESYNC_REQUIRED: u32 = 1 << 5;
}

/// In-RAM mirror of account + storage state. Concurrent readers (the validator); writers are the
/// boot prime + the event-applier. Backed by one `RwLock<AHashMap>` for simplicity (upgradeable
/// to a sharded / lock-free store for the live perf tier).
///
/// `block` tags which block the mirrored state is consistent for; a read-through fetcher trusts a
/// hit only when its target block matches, so stale state is never served (it falls back to RPC).
#[derive(Default)]
pub struct StateMirror {
    accounts: RwLock<AHashMap<[u8; 20], AccountEntry>>,
    block: AtomicU64,
    /// [WP2 · Fix C] Runtime fail-safe: a bitset of blocking [`health_reason`]s. Set when a subsystem
    /// proves the mirror cannot be trusted (repin-audit divergence, proof-unresolved debt, queue gap,
    /// reorg recovery, full-resync demand). While ANY bit is set a `MirroredStateFetcher` treats the
    /// mirror as untrusted (every read → RPC), so a drifted/unproven mirror can NEVER silently serve
    /// stale state to the validator. `0` = healthy, so `#[derive(Default)]` yields "healthy". Each bit
    /// is cleared ONLY by the subsystem that owns it — no blanket clear.
    reasons: std::sync::atomic::AtomicU32,
    /// [B1] The block-boundary PUBLISHED snapshot — the validator-visible, frozen view of the mirror.
    ///
    /// The advancer mutates `accounts` (its WORKING map) continuously, slot-by-slot, as it applies
    /// the in-progress block; the validator reads THIS immutable snapshot instead, captured once per
    /// validation (via [`MirroredStateFetcher::new_published`]). It is refreshed only at block close
    /// ([`publish`](Self::publish)) with the just-closed block's complete, reconciled state, so a
    /// validation can never observe a cross-slot mix of two blocks (a torn read). The `block` tag
    /// lives INSIDE the snapshot, so it flips atomically WITH the content on the [`ArcSwapOption`]
    /// store — trust (`snap.block == fork_pin`) and content are always mutually consistent. `None`
    /// until the first close; readers then simply fall through to RPC (fail-safe). Default empty ⇒
    /// with `MIRROR_FEED_ADVANCE`/`REVM_STATE` off nothing is ever published or read (byte-identical).
    published: ArcSwapOption<MirrorSnapshot>,
    /// [WP2 · Fix C] Monotonic publish generation, stamped into each published [`MirrorSnapshot`].
    generation: AtomicU64,
}

/// [B1] An immutable, block-consistent snapshot of the mirror's storage, published atomically at
/// block close and captured by the validator for the whole of one validation. Because it is frozen,
/// every read a validation makes observes ONE block's state — the linchpin of "no torn read": a
/// concurrent [`StateMirror::publish`] swaps in a NEW `Arc`, leaving any already-captured snapshot
/// untouched. The `block` tag is captured together with the content, so a reader that trusts the
/// snapshot for `block` reads content that is exactly that block's state (never a partial advance).
pub struct MirrorSnapshot {
    block: u64,
    accounts: AHashMap<[u8; 20], AccountEntry>,
    /// [WP2 · Fix C] Whether this snapshot was published by a CERTIFIED close (a complete, reason-free
    /// reconcile). A validator trusts a snapshot only when it is certified AND its block tag matches —
    /// so a snapshot frozen by an incomplete/failed path can never become trusted, even if the mirror
    /// later heals a different reason. Defense in depth on top of the publish gate.
    certified: bool,
    /// [WP2 · Fix C] Monotonic publish generation (immutable per snapshot). Lets an observer
    /// distinguish two certified snapshots of the same block and detect a stale capture.
    generation: u64,
}

impl MirrorSnapshot {
    /// The block this snapshot's state is consistent for.
    pub fn block(&self) -> u64 {
        self.block
    }

    /// [WP2 · Fix C] Whether this snapshot was published by a certified (complete, reason-free) close.
    pub fn certified(&self) -> bool {
        self.certified
    }

    /// [WP2 · Fix C] The immutable publish generation of this snapshot.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// `(balance, nonce, code)` iff the account's data was primed in the snapshot, else `None`
    /// (same "never fabricate account data from a storage-only entry" rule as the live mirror).
    pub fn get_account(&self, addr: [u8; 20]) -> Option<(U256, u64, Option<Vec<u8>>)> {
        self.accounts
            .get(&addr)
            .filter(|e| e.has_account_data)
            .map(|e| (e.balance, e.nonce, e.code.clone()))
    }

    /// The snapshot's storage value for `(addr, slot)`, else `None`.
    pub fn get_storage(&self, addr: [u8; 20], slot: U256) -> Option<U256> {
        self.accounts
            .get(&addr)
            .and_then(|e| e.storage.get(&slot).map(|s| s.value))
    }
}

impl StateMirror {
    /// Empty mirror (no block-consistent state until [`set_block`](Self::set_block)).
    pub fn new() -> Self {
        Self::default()
    }

    /// The block this mirror's state is consistent for (`0` = none/unprimed).
    pub fn block(&self) -> u64 {
        self.block.load(Ordering::Acquire)
    }

    /// Declare the mirror consistent for `block`. Call after priming/applying that block's state.
    pub fn set_block(&self, block: u64) {
        self.block.store(block, Ordering::Release);
    }

    /// Retag the mirror to a NEW fork pin. When the block actually changes, every
    /// `Fallback`-sourced storage entry is purged first: those values were write-through-cached
    /// from the OLD pin's RPC fetcher and are only valid there — serving them at a newer pin is
    /// exactly the "ancient value survives repins forever" poison. `Shadow`-sourced entries
    /// persist (head-fresh by construction; the whole point of the standing mirror). Retagging
    /// to the SAME block keeps everything (same pin ⇒ fallback values still canonical).
    pub fn retag_block(&self, block: u64) {
        if self.block() != block {
            self.purge_fallback_storage();
        }
        self.set_block(block);
    }

    /// Drop every `Fallback`-sourced storage entry (and any account entry left with neither
    /// account data nor slots). See [`retag_block`](Self::retag_block).
    fn purge_fallback_storage(&self) {
        let mut a = self.accounts.write().unwrap();
        a.retain(|_, e| {
            e.storage.retain(|_, s| s.source == SlotSource::Shadow);
            e.has_account_data || !e.storage.is_empty()
        });
    }

    /// Whether the mirror is currently trusted to serve reads — true ONLY when NO blocking
    /// [`health_reason`] is set. Cheap relaxed load — hot-path safe.
    pub fn is_healthy(&self) -> bool {
        self.reasons.load(Ordering::Relaxed) == 0
    }

    /// [WP2 · Fix C] The current blocking-reason bitset (telemetry / test inspection).
    pub fn health_reasons(&self) -> u32 {
        self.reasons.load(Ordering::Relaxed)
    }

    /// [WP2 · Fix C] Whether ALL of `reasons` (a [`health_reason`] mask) are currently set.
    pub fn has_reason(&self, reasons: u32) -> bool {
        self.reasons.load(Ordering::Relaxed) & reasons == reasons
    }

    /// [WP2 · Fix C] SET the given blocking [`health_reason`](s) — the mirror stops being trusted until
    /// the owning subsystem clears exactly that reason. OR-ing bits, so setting one reason never
    /// disturbs another.
    pub fn set_reason(&self, reasons: u32) {
        self.reasons.fetch_or(reasons, Ordering::Relaxed);
    }

    /// [WP2 · Fix C] CLEAR the given blocking [`health_reason`](s) — AND-NOT, so a subsystem clears
    /// ONLY the reason it proved resolved and can never erase another subsystem's trip (this is what
    /// prevents laundering a proof/recovery trip via an unrelated clean signal).
    pub fn clear_reason(&self, reasons: u32) {
        self.reasons.fetch_and(!reasons, Ordering::Relaxed);
    }

    /// [B1] PUBLISH the current working state as the validator-visible snapshot, tagged `block`.
    ///
    /// Called by the advancer at **block close only** — AFTER the closed block's canonical reconcile
    /// so the published state is complete + repaired. It clones the working `accounts` map under one
    /// read-lock and atomically stores the resulting immutable [`MirrorSnapshot`] (block tag + content
    /// together) into [`published`](Self::published). A validator that captured a prior snapshot keeps
    /// reading it (frozen); a validator constructing a fetcher AFTER this call sees the new block.
    /// Never observed half-applied: the advancer mutates the working map only BETWEEN publishes, and
    /// the store is a single atomic pointer swap. The clone is O(working-set) once per block (~250 ms
    /// cadence); the advancer mirror's accounts are storage-only (no bytecode), so it is cheap.
    ///
    /// [WP2 · Fix C] `certified` marks whether the caller published from a COMPLETE, reason-free close
    /// (the publish gate). The validator ([`MirroredStateFetcher::new_published`]) trusts a snapshot
    /// ONLY when it is certified AND its block tag matches — so a snapshot frozen by an
    /// incomplete/failed path (`certified=false`) can never become trusted, even after a later heal.
    fn publish_inner(&self, block: u64, certified: bool) {
        let accounts = self.accounts.read().unwrap().clone();
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.published.store(Some(Arc::new(MirrorSnapshot {
            block,
            accounts,
            certified,
            generation,
        })));
    }

    /// [WP2 · Fix C] Publish a CERTIFIED snapshot — the caller has confirmed a complete, reason-free
    /// close (the publish gate in `close_block`/reorg). Only certified snapshots are ever trusted.
    pub fn publish_certified(&self, block: u64) {
        self.publish_inner(block, true);
    }

    /// [B1] Publish the working state as an UNCERTIFIED snapshot. Retained for the pre-WP2 API surface
    /// and defense-in-depth: an uncertified snapshot is NEVER trusted by `new_published`, so a caller
    /// that has not passed the publish gate cannot make state validator-visible.
    pub fn publish(&self, block: u64) {
        self.publish_inner(block, false);
    }

    /// [B1] The current validator-visible snapshot (`None` until the first [`publish`](Self::publish)).
    /// A lock-free `Arc` load; the caller holds the returned snapshot frozen for its whole validation.
    pub fn published(&self) -> Option<Arc<MirrorSnapshot>> {
        self.published.load_full()
    }

    /// Publish an account's balance/nonce/code (preserving any cached storage slots).
    pub fn set_account(&self, addr: [u8; 20], balance: U256, nonce: u64, code: Option<Vec<u8>>) {
        let mut a = self.accounts.write().unwrap();
        let e = a.entry(addr).or_default();
        e.has_account_data = true;
        e.balance = balance;
        e.nonce = nonce;
        e.code = code;
    }

    /// Cache one HEAD-FRESH storage slot (shadow-published post-swap state; persists across
    /// repins). Does NOT imply the account's balance/nonce/code are present.
    pub fn set_storage(&self, addr: [u8; 20], slot: U256, value: U256) {
        let mut a = self.accounts.write().unwrap();
        a.entry(addr).or_default().storage.insert(
            slot,
            SlotEntry {
                value,
                source: SlotSource::Shadow,
            },
        );
    }

    /// Cache one PIN-BLOCK storage slot (the read-through fetcher's miss write-back). Valid only
    /// for the current fork pin — purged by [`retag_block`](Self::retag_block) on the next repin.
    pub fn set_storage_fallback(&self, addr: [u8; 20], slot: U256, value: U256) {
        let mut a = self.accounts.write().unwrap();
        // Never demote a head-fresh shadow value to a pin-block one: the fetcher only write-backs
        // on a miss, but keep the invariant explicit against future callers.
        match a.entry(addr).or_default().storage.entry(slot) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                if e.get().source == SlotSource::Fallback {
                    e.insert(SlotEntry {
                        value,
                        source: SlotSource::Fallback,
                    });
                }
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(SlotEntry {
                    value,
                    source: SlotSource::Fallback,
                });
            }
        }
    }

    /// Publish one replay's FULL changed-slot set under a SINGLE write-lock acquisition, so a
    /// concurrent validator read can never observe a half-applied tx (per-slot locking let a
    /// reader interleave between a pool's slot0 and its liquidity slot — a torn post-state).
    /// Shadow-sourced: these are head-fresh post-swap values.
    pub fn apply_batch<I: IntoIterator<Item = ([u8; 20], U256, U256)>>(&self, slots: I) {
        let mut a = self.accounts.write().unwrap();
        for (addr, slot, value) in slots {
            a.entry(addr).or_default().storage.insert(
                slot,
                SlotEntry {
                    value,
                    source: SlotSource::Shadow,
                },
            );
        }
    }

    /// [WP4 · Fix E] The checked-reconcile commit boundary: apply a batch whose VALUES are
    /// [`engine_revm::Proven`] — i.e. every value flowed through the checked canonical-fetch seam. A
    /// raw value obtained from the infallible `fetch_storage` has type `U256`, NOT `Proven<U256>`, so it
    /// cannot be committed here; the checked reconcile core (normal close, debt recovery, deep-reorg)
    /// commits through this method, making a fabricated-on-error value type-impossible to apply.
    pub fn apply_checked_batch<I>(&self, slots: I)
    where
        I: IntoIterator<Item = ([u8; 20], U256, engine_revm::Proven<U256>)>,
    {
        self.apply_batch(
            slots
                .into_iter()
                .map(|(addr, slot, value)| (addr, slot, value.into_inner())),
        );
    }

    /// `(balance, nonce, code)` iff the account's data has been primed, else `None`.
    pub fn get_account(&self, addr: [u8; 20]) -> Option<(U256, u64, Option<Vec<u8>>)> {
        let a = self.accounts.read().unwrap();
        a.get(&addr)
            .filter(|e| e.has_account_data)
            .map(|e| (e.balance, e.nonce, e.code.clone()))
    }

    /// The cached storage value iff this `(account, slot)` has been mirrored, else `None`.
    pub fn get_storage(&self, addr: [u8; 20], slot: U256) -> Option<U256> {
        let a = self.accounts.read().unwrap();
        a.get(&addr).and_then(|e| e.storage.get(&slot).map(|s| s.value))
    }

    /// Number of mirrored accounts (primed or storage-only).
    pub fn account_count(&self) -> usize {
        self.accounts.read().unwrap().len()
    }

    /// [WP5 · WP4 carry-over] The FULL resident storage footprint — every `(addr, slot)` currently held.
    /// Used by the in-run full-resync recovery to re-prove the entire mirror at one current checked
    /// anchor before clearing `FULL_RESYNC_REQUIRED`. Single read-lock snapshot; unbounded (a full
    /// resync re-proves everything), so it is a rare recovery path, not a hot-loop call.
    pub fn resident_footprint(&self) -> Vec<([u8; 20], U256)> {
        let a = self.accounts.read().unwrap();
        let mut out = Vec::with_capacity(a.values().map(|e| e.storage.len()).sum());
        for (addr, entry) in a.iter() {
            for slot in entry.storage.keys() {
                out.push((*addr, *slot));
            }
        }
        out
    }

    /// Sample up to `max` mirrored `(addr, slot, value)` storage entries for the Stage-2 differential
    /// audit (compare each against a fresh canonical RPC read at a repin — the one moment mirror and
    /// RPC are at the same head). Bounded, single read-lock snapshot; order is arbitrary but stable
    /// within a call. Empty when the mirror is empty.
    pub fn sample_storage(&self, max: usize) -> Vec<([u8; 20], U256, U256)> {
        let a = self.accounts.read().unwrap();
        let mut out = Vec::with_capacity(max.min(64));
        for (addr, e) in a.iter() {
            for (slot, s) in e.storage.iter() {
                if out.len() >= max {
                    return out;
                }
                out.push((*addr, *slot, s.value));
            }
        }
        out
    }

    /// Serialize the whole mirror (block tag + all accounts + storage) to `path`, atomically
    /// (temp file + rename). This is the PB-STATE artifact: persist a block-consistent snapshot at
    /// shutdown so the next boot loads RAM-state instantly instead of re-fetching ~3.6k pools over
    /// RPC (~8 min). The loader is fingerprint-free — correctness comes from the block tag + a
    /// live delta-refresh of changed pools (AWS-gated), not from trusting the file blindly.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let body = self.encode_body();
        let checksum = fnv1a(&body);
        let mut buf = Vec::with_capacity(body.len() + 28);
        buf.extend_from_slice(STATE_MAGIC);
        buf.extend_from_slice(&STATE_VERSION.to_le_bytes());
        buf.extend_from_slice(&checksum.to_le_bytes());
        buf.extend_from_slice(&(body.len() as u64).to_le_bytes());
        buf.extend_from_slice(&body);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &buf)?;
        std::fs::rename(&tmp, path)
    }

    /// Load a mirror previously [`save`](Self::save)d. Returns `None` (never an error/panic) on a
    /// missing/short/corrupt/version-mismatched file so a bad cache simply degrades to a cold boot.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        let buf = std::fs::read(path).ok()?;
        if buf.len() < 28 || &buf[0..8] != STATE_MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(buf[8..12].try_into().ok()?);
        if version != STATE_VERSION {
            return None;
        }
        let checksum = u64::from_le_bytes(buf[12..20].try_into().ok()?);
        let body_len = u64::from_le_bytes(buf[20..28].try_into().ok()?) as usize;
        let body = buf.get(28..28 + body_len)?;
        if fnv1a(body) != checksum {
            return None;
        }
        Self::decode_body(body)
    }

    /// Encode `block` + every account/storage entry to a self-describing little-endian byte body.
    fn encode_body(&self) -> Vec<u8> {
        let a = self.accounts.read().unwrap();
        let mut b = Vec::new();
        b.extend_from_slice(&self.block().to_le_bytes());
        b.extend_from_slice(&(a.len() as u64).to_le_bytes());
        for (addr, e) in a.iter() {
            b.extend_from_slice(addr);
            b.push(e.has_account_data as u8);
            b.extend_from_slice(&e.balance.to_le_bytes::<32>());
            b.extend_from_slice(&e.nonce.to_le_bytes());
            match &e.code {
                None => b.push(0),
                Some(code) => {
                    b.push(1);
                    b.extend_from_slice(&(code.len() as u64).to_le_bytes());
                    b.extend_from_slice(code);
                }
            }
            b.extend_from_slice(&(e.storage.len() as u64).to_le_bytes());
            for (slot, s) in e.storage.iter() {
                b.extend_from_slice(&slot.to_le_bytes::<32>());
                b.extend_from_slice(&s.value.to_le_bytes::<32>());
            }
        }
        b
    }

    /// Inverse of [`encode_body`](Self::encode_body); `None` on any truncation/overrun.
    fn decode_body(body: &[u8]) -> Option<Self> {
        let mut p = 0usize;
        let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
            let s = body.get(*p..*p + n)?;
            *p += n;
            Some(s)
        };
        let block = u64::from_le_bytes(take(&mut p, 8)?.try_into().ok()?);
        let n_accounts = u64::from_le_bytes(take(&mut p, 8)?.try_into().ok()?) as usize;
        let mut accounts: AHashMap<[u8; 20], AccountEntry> = AHashMap::with_capacity(n_accounts);
        for _ in 0..n_accounts {
            let addr: [u8; 20] = take(&mut p, 20)?.try_into().ok()?;
            let has_account_data = take(&mut p, 1)?[0] != 0;
            let balance = U256::from_le_slice(take(&mut p, 32)?);
            let nonce = u64::from_le_bytes(take(&mut p, 8)?.try_into().ok()?);
            let code = match take(&mut p, 1)?[0] {
                0 => None,
                _ => {
                    let len = u64::from_le_bytes(take(&mut p, 8)?.try_into().ok()?) as usize;
                    Some(take(&mut p, len)?.to_vec())
                }
            };
            let n_storage = u64::from_le_bytes(take(&mut p, 8)?.try_into().ok()?) as usize;
            let mut storage: AHashMap<U256, SlotEntry> = AHashMap::with_capacity(n_storage);
            for _ in 0..n_storage {
                let slot = U256::from_le_slice(take(&mut p, 32)?);
                let val = U256::from_le_slice(take(&mut p, 32)?);
                // Disk-loaded values were canonical at the SAVED block tag — the same freshness
                // class as a pin write-back, so they purge on the first retag to a different pin
                // instead of masquerading as head-fresh state.
                storage.insert(
                    slot,
                    SlotEntry {
                        value: val,
                        source: SlotSource::Fallback,
                    },
                );
            }
            accounts.insert(
                addr,
                AccountEntry {
                    has_account_data,
                    balance,
                    nonce,
                    code,
                    storage,
                },
            );
        }
        Some(Self {
            accounts: RwLock::new(accounts),
            block: AtomicU64::new(block),
            reasons: std::sync::atomic::AtomicU32::new(0),
            // A disk-loaded mirror has no published snapshot yet: the advancer publishes at the
            // first block close, and until then the validator (if any) falls through to RPC.
            published: ArcSwapOption::default(),
            generation: AtomicU64::new(0),
        })
    }
}

const STATE_MAGIC: &[u8; 8] = b"MNTSSTAT";
const STATE_VERSION: u32 = 1;

/// FNV-1a 64-bit over `data` (corruption guard; not cryptographic).
fn fnv1a(data: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &byte in data {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Read-through fetcher: serve from `mirror`, fall back to `fallback` (RPC) on a miss and write
/// the result back. A mirror hit avoids the fallback entirely — that is the bottleneck fix.
///
/// Constructed for one `target_block` (the block the REVM fork validates at). The mirror is
/// trusted only when it is consistent for that block; otherwise EVERY read goes to the fallback,
/// so the validator can never read stale cross-block state. When mistrusted, misses are NOT
/// written back (they'd pollute a block the mirror isn't tracking).
pub struct MirroredStateFetcher<'a, F: StateFetcher> {
    mirror: &'a StateMirror,
    fallback: F,
    trusted: bool,
    /// [B1] `Some` ⇒ read the frozen published SNAPSHOT (the torn-read-free validator path,
    /// [`new_published`](Self::new_published)); `None` ⇒ read the LIVE map (the legacy path, byte-
    /// identical to pre-B1 — used by the rpc-mode wiring and the single-threaded tests). Captured
    /// once at construction, so every read of one validation observes ONE consistent block.
    snap: Option<Arc<MirrorSnapshot>>,
}

impl<'a, F: StateFetcher> MirroredStateFetcher<'a, F> {
    /// Wrap a mirror over a fallback fetcher (typically the RPC fork fetcher) for `target_block`,
    /// reading the mirror's LIVE map. Unchanged since pre-B1: safe only where no advancer mutates
    /// the map concurrently (the rpc-mode legacy mirror is untrusted; tests are single-threaded).
    /// Under `REVM_STATE=mirror` the validator uses [`new_published`](Self::new_published) instead.
    pub fn new(mirror: &'a StateMirror, fallback: F, target_block: u64) -> Self {
        let trusted = target_block != NO_BLOCK && mirror.block() == target_block;
        Self {
            mirror,
            fallback,
            trusted,
            snap: None,
        }
    }

    /// [B1] Wrap a mirror over a fallback fetcher, reading the block-boundary PUBLISHED snapshot
    /// (frozen end-of-block state) instead of the advancer's live map. Captures the snapshot ONCE
    /// here, so every read this fetcher serves for the whole validation observes that one block —
    /// never a cross-slot mix of the block the advancer is concurrently applying (a torn read).
    ///
    /// Trusted iff the captured snapshot's block equals `target_block` (the fork's RPC pin): then a
    /// snapshot HIT and an RPC-fallback MISS are the SAME block, so the two can never disagree across
    /// blocks. No snapshot / block mismatch / (live) unhealthy ⇒ untrusted ⇒ every read falls to the
    /// fork's RPC at its pin — byte-identical to the pure-RPC fork (fail-safe).
    pub fn new_published(mirror: &'a StateMirror, fallback: F, target_block: u64) -> Self {
        let snap = mirror.published();
        // [WP2 · Fix C] Trust a snapshot ONLY when it is CERTIFIED (published by a complete, reason-free
        // close) AND its block tag matches the pin. An uncertified snapshot — frozen by an
        // incomplete/failed path — is never trusted, even if the mirror later heals another reason.
        let trusted = target_block != NO_BLOCK
            && snap.as_ref().is_some_and(|s| s.block == target_block && s.certified);
        Self {
            mirror,
            fallback,
            trusted,
            snap,
        }
    }

    /// Whether the mirror is being trusted for this fetcher's target block.
    pub fn is_trusted(&self) -> bool {
        self.trusted
    }

    /// The mirror storage this fetcher reads on a trusted hit: the frozen snapshot ([`new_published`])
    /// or the live map ([`new`]). One place so the three read methods share the exact same source.
    #[inline]
    fn mirror_storage(&self, address: [u8; 20], slot: U256) -> Option<U256> {
        match &self.snap {
            Some(s) => s.get_storage(address, slot),
            None => self.mirror.get_storage(address, slot),
        }
    }

    /// The mirror account this fetcher reads on a trusted hit (snapshot or live map).
    #[inline]
    fn mirror_account(&self, address: [u8; 20]) -> Option<(U256, u64, Option<Vec<u8>>)> {
        match &self.snap {
            Some(s) => s.get_account(address),
            None => self.mirror.get_account(address),
        }
    }
}

impl<F: StateFetcher> StateFetcher for MirroredStateFetcher<'_, F> {
    fn fetch_account(&self, address: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
        if !self.trusted || !self.mirror.is_healthy() {
            return self.fallback.fetch_account(address);
        }
        if let Some(hit) = self.mirror_account(address) {
            return hit;
        }
        let r = self.fallback.fetch_account(address);
        // Legacy live path caches the miss; the published-snapshot path is READ-ONLY — the validator
        // never mutates the advancer's working map (isolation). A snapshot miss just re-reads RPC.
        if self.snap.is_none() {
            self.mirror.set_account(address, r.0, r.1, r.2.clone());
        }
        r
    }

    fn fetch_storage(&self, address: [u8; 20], slot: U256) -> U256 {
        if !self.trusted || !self.mirror.is_healthy() {
            return self.fallback.fetch_storage(address, slot);
        }
        if let Some(v) = self.mirror_storage(address, slot) {
            return v;
        }
        let v = self.fallback.fetch_storage(address, slot);
        // Pin-block write-back (legacy live path only): valid only for THIS pin — tagged so the next
        // retag purges it (an untagged write-back survived repins and served ancient state at newer
        // pins). The published-snapshot path is read-only (see `fetch_account`).
        if self.snap.is_none() {
            self.mirror.set_storage_fallback(address, slot, v);
        }
        v
    }

    /// [Stage 2 · CacheOnly] Non-blocking storage read (only consulted by a cache-only `ForkDb`).
    ///
    /// * Mirror untrusted (block tag ≠ target) OR unhealthy ⇒ **degrade to blocking-RPC**: serve the
    ///   fallback and return `Some` (NEVER abort). This is the plan's "unhealthy-in-cache-only degrades
    ///   to blocking-RPC mode, not mass-PendingState starvation" — a fail-SAFE, not a fail-fast.
    /// * Trusted + healthy: a mirror hit is served; a MISS returns `None` so the fork DB aborts the
    ///   validation and records the slot for the async warmer — with NO pin-block write-back (the
    ///   abort path must not pollute the mirror with a fallback value it never fetched).
    fn try_fetch_storage(&self, address: [u8; 20], slot: U256) -> Option<U256> {
        if !self.trusted || !self.mirror.is_healthy() {
            return Some(self.fallback.fetch_storage(address, slot));
        }
        self.mirror_storage(address, slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A fake fetcher that counts how many times it's hit (proxy for "RPC round-trips").
    struct MockFetcher {
        account_calls: AtomicU64,
        storage_calls: AtomicU64,
        bal: U256,
        nonce: u64,
        code: Option<Vec<u8>>,
        slot_val: U256,
    }
    impl StateFetcher for MockFetcher {
        fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            self.account_calls.fetch_add(1, Ordering::Relaxed);
            (self.bal, self.nonce, self.code.clone())
        }
        fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
            self.storage_calls.fetch_add(1, Ordering::Relaxed);
            self.slot_val
        }
    }
    fn mock(bal: u64, nonce: u64, code: Option<Vec<u8>>, slot_val: u64) -> MockFetcher {
        MockFetcher {
            account_calls: AtomicU64::new(0),
            storage_calls: AtomicU64::new(0),
            bal: U256::from(bal),
            nonce,
            code,
            slot_val: U256::from(slot_val),
        }
    }

    #[test]
    fn mirror_hit_avoids_fallback_rpc() {
        let mirror = StateMirror::new();
        let addr = [0x11; 20];
        mirror.set_account(addr, U256::from(5u64), 7, Some(vec![1, 2, 3]));
        mirror.set_storage(addr, U256::from(9u64), U256::from(42u64));
        mirror.set_block(100);
        let f = MirroredStateFetcher::new(&mirror, mock(999, 0, None, 0), 100);
        assert!(f.is_trusted());
        let (b, n, c) = f.fetch_account(addr);
        assert_eq!((b, n, c), (U256::from(5u64), 7, Some(vec![1, 2, 3])));
        assert_eq!(f.fetch_storage(addr, U256::from(9u64)), U256::from(42u64));
        // THE bottleneck fix: a fully-mirrored read does ZERO fallback RPC.
        assert_eq!(f.fallback.account_calls.load(Ordering::Relaxed), 0);
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 0);
    }

    // ── Stage 2 · CacheOnly try_fetch_storage semantics ───────────────────────────────────────

    #[test]
    fn try_fetch_storage_trusted_hit_serves_without_fallback() {
        let mirror = StateMirror::new();
        let addr = [0x11; 20];
        mirror.set_storage(addr, U256::from(9u64), U256::from(42u64));
        mirror.set_block(100);
        let f = MirroredStateFetcher::new(&mirror, mock(999, 0, None, 777), 100);
        assert!(f.is_trusted());
        assert_eq!(f.try_fetch_storage(addr, U256::from(9u64)), Some(U256::from(42u64)));
        assert_eq!(
            f.fallback.storage_calls.load(Ordering::Relaxed),
            0,
            "a trusted mirror hit never touches the fallback"
        );
    }

    #[test]
    fn try_fetch_storage_trusted_miss_aborts_and_does_not_writeback() {
        // The core cache-only abort signal: a trusted+healthy MISS returns None (→ ForkDb aborts),
        // with NO pin-block write-back (unlike the blocking fetch_storage, which caches the fallback).
        let mirror = StateMirror::new();
        let addr = [0x22; 20];
        mirror.set_block(100);
        let f = MirroredStateFetcher::new(&mirror, mock(0, 0, None, 777), 100);
        assert!(f.is_trusted());
        assert_eq!(f.try_fetch_storage(addr, U256::from(5u64)), None, "miss ⇒ abort signal");
        assert_eq!(
            f.fallback.storage_calls.load(Ordering::Relaxed),
            0,
            "the abort path must not consult the RPC fallback"
        );
        assert_eq!(
            mirror.get_storage(addr, U256::from(5u64)),
            None,
            "the abort path must not write a fallback value into the mirror"
        );
    }

    #[test]
    fn try_fetch_storage_untrusted_degrades_to_blocking_not_abort() {
        // Untrusted (block mismatch): cache-only must DEGRADE to blocking-RPC (Some), never abort —
        // otherwise an untrusted mirror would starve every validation with PendingState.
        let mirror = StateMirror::new();
        let addr = [0x33; 20];
        mirror.set_block(100);
        let f = MirroredStateFetcher::new(&mirror, mock(0, 0, None, 777), 101); // fork ≠ mirror block
        assert!(!f.is_trusted());
        assert_eq!(
            f.try_fetch_storage(addr, U256::from(5u64)),
            Some(U256::from(777u64)),
            "untrusted ⇒ blocking fallback, never None"
        );
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn try_fetch_storage_unhealthy_degrades_to_blocking_not_abort() {
        // Healthy trip (audit-tripped mirror): same fail-safe — blocking-RPC, never abort.
        let mirror = StateMirror::new();
        let addr = [0x44; 20];
        mirror.set_storage(addr, U256::from(5u64), U256::from(42u64));
        mirror.set_block(100);
        mirror.set_reason(health_reason::GROSS_DIVERGENCE);
        let f = MirroredStateFetcher::new(&mirror, mock(0, 0, None, 777), 100);
        assert!(f.is_trusted(), "trusted flag is block-tag only; health is a runtime gate");
        assert_eq!(
            f.try_fetch_storage(addr, U256::from(5u64)),
            Some(U256::from(777u64)),
            "unhealthy ⇒ blocking fallback (NOT the stale mirror hit, NOT an abort)"
        );
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn mirror_miss_falls_back_once_then_caches() {
        let mirror = StateMirror::new();
        let addr = [0x22; 20];
        mirror.set_block(100);
        let f = MirroredStateFetcher::new(&mirror, mock(123, 3, Some(vec![9]), 77), 100);
        // miss → fallback (1 call), write-through cached
        assert_eq!(f.fetch_account(addr).0, U256::from(123u64));
        assert_eq!(f.fallback.account_calls.load(Ordering::Relaxed), 1);
        // hit → no new fallback
        assert_eq!(f.fetch_account(addr).0, U256::from(123u64));
        assert_eq!(f.fallback.account_calls.load(Ordering::Relaxed), 1);
        // storage: miss once, then cached
        assert_eq!(f.fetch_storage(addr, U256::from(5u64)), U256::from(77u64));
        assert_eq!(f.fetch_storage(addr, U256::from(5u64)), U256::from(77u64));
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn stale_block_serves_no_mirror_hits() {
        // Mirror is primed + consistent for block 100, but the fork validates at 101.
        let mirror = StateMirror::new();
        let addr = [0x44; 20];
        mirror.set_account(addr, U256::from(5u64), 7, Some(vec![1]));
        mirror.set_storage(addr, U256::from(9u64), U256::from(42u64));
        mirror.set_block(100);
        let f = MirroredStateFetcher::new(&mirror, mock(999, 1, Some(vec![2]), 77), 101);
        assert!(!f.is_trusted()); // block mismatch → mirror NOT trusted
                                  // Reads must come from the fallback (fresh block), NOT the stale mirror.
        assert_eq!(f.fetch_account(addr).0, U256::from(999u64));
        assert_eq!(f.fetch_storage(addr, U256::from(9u64)), U256::from(77u64));
        assert_eq!(f.fallback.account_calls.load(Ordering::Relaxed), 1);
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 1);
        // And the stale mirror was not overwritten with the other block's data.
        assert_eq!(
            mirror.get_storage(addr, U256::from(9u64)),
            Some(U256::from(42u64))
        );
    }

    #[test]
    fn block_zero_is_never_trusted() {
        let mirror = StateMirror::new(); // block defaults to 0
        let f = MirroredStateFetcher::new(&mirror, mock(1, 1, None, 1), 0);
        assert!(!f.is_trusted());
    }

    #[test]
    fn storage_only_account_does_not_fabricate_account_data() {
        let mirror = StateMirror::new();
        let addr = [0x33; 20];
        mirror.set_storage(addr, U256::from(1u64), U256::from(2u64));
        // account data NOT primed → must miss (so a fetch falls back for the real balance/code)
        assert!(mirror.get_account(addr).is_none());
        // but the cached slot is served
        assert_eq!(
            mirror.get_storage(addr, U256::from(1u64)),
            Some(U256::from(2u64))
        );
    }

    #[test]
    fn retag_purges_fallback_entries_keeps_shadow_entries() {
        // [local-state D5] Shadow-published slots are head-fresh and survive a repin; write-back
        // slots were canonical at the OLD pin only and must NOT be served at the new one.
        let m = StateMirror::new();
        let addr = [0x55; 20];
        m.set_storage(addr, U256::from(1u64), U256::from(11u64)); // shadow (head-fresh)
        m.set_storage_fallback(addr, U256::from(2u64), U256::from(22u64)); // pin-block
        m.set_block(100);
        // Same-block retag: the pin didn't move, fallback values are still canonical — keep all.
        m.retag_block(100);
        assert_eq!(m.get_storage(addr, U256::from(2u64)), Some(U256::from(22u64)));
        // New-block retag: fallback purged, shadow persists.
        m.retag_block(101);
        assert_eq!(
            m.get_storage(addr, U256::from(1u64)),
            Some(U256::from(11u64)),
            "shadow (head-fresh) entries persist across repins"
        );
        assert_eq!(
            m.get_storage(addr, U256::from(2u64)),
            None,
            "pin-block write-backs must not survive a repin"
        );
        assert_eq!(m.block(), 101);
    }

    #[test]
    fn retag_prunes_accounts_left_empty_by_the_purge() {
        let m = StateMirror::new();
        let fallback_only = [0x66; 20];
        m.set_storage_fallback(fallback_only, U256::from(1u64), U256::from(1u64));
        let primed = [0x77; 20];
        m.set_account(primed, U256::from(5u64), 1, None);
        m.set_storage_fallback(primed, U256::from(9u64), U256::from(9u64));
        m.set_block(10);
        m.retag_block(11);
        // The storage-only fallback account is gone entirely; the primed account keeps its
        // account data (block-invariant code/balance priming) but loses the stale slot.
        assert_eq!(m.account_count(), 1);
        assert!(m.get_account(primed).is_some());
        assert_eq!(m.get_storage(primed, U256::from(9u64)), None);
    }

    #[test]
    fn write_through_miss_is_pin_scoped_not_head_fresh() {
        // End-to-end D5: a trusted-fetcher miss write-back must disappear on the next repin —
        // the fallback serves the NEW pin's value instead of the cached old-pin one.
        let mirror = StateMirror::new();
        let addr = [0x88; 20];
        mirror.set_block(100);
        let f = MirroredStateFetcher::new(&mirror, mock(0, 0, None, 500), 100);
        assert_eq!(f.fetch_storage(addr, U256::from(3u64)), U256::from(500u64));
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 1);
        // Repin → retag: the write-back is purged; a fresh fetcher must re-consult its fallback
        // (which now serves the new pin's value) instead of the stale cached 500.
        mirror.retag_block(101);
        let f2 = MirroredStateFetcher::new(&mirror, mock(0, 0, None, 777), 101);
        assert_eq!(
            f2.fetch_storage(addr, U256::from(3u64)),
            U256::from(777u64),
            "old-pin write-back must not be served at the new pin"
        );
        assert_eq!(f2.fallback.storage_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn apply_batch_publishes_all_slots_as_head_fresh() {
        // [local-state D6] One replay's slots land under a single write-lock acquisition (no
        // torn tx visible to a concurrent reader) and are shadow-sourced (survive retag).
        let m = StateMirror::new();
        let pool = [0x99; 20];
        let token = [0x9a; 20];
        m.apply_batch([
            (pool, U256::from(0u64), U256::from(111u64)),
            (pool, U256::from(4u64), U256::from(222u64)),
            (token, U256::from(7u64), U256::from(333u64)),
        ]);
        m.set_block(50);
        m.retag_block(51); // survives: batch entries are head-fresh
        assert_eq!(m.get_storage(pool, U256::from(0u64)), Some(U256::from(111u64)));
        assert_eq!(m.get_storage(pool, U256::from(4u64)), Some(U256::from(222u64)));
        assert_eq!(m.get_storage(token, U256::from(7u64)), Some(U256::from(333u64)));
    }

    // ── [B1] Torn mirror reads — block-boundary published snapshot ─────────────────────────────
    // The Stage-1 advancer mutates the mirror's LIVE map slot-by-slot for the block it is applying,
    // while the validator reads it slot-by-slot for one validation. Reading the live map, a single
    // validation can observe slot A at block N-1 and slot B at block N — a CROSS-SLOT state that
    // existed at NO block (a torn read). Fix: the advancer PUBLISHES a frozen snapshot only at block
    // close; the validator captures that immutable snapshot once and reads every slot from it, so it
    // always sees ONE consistent block. These tests prove the snapshot never tears and pin the trust
    // rule (`snap.block == fork_pin`), while the legacy live-map fetcher (unchanged) demonstrably can.

    #[test]
    fn published_snapshot_is_consistent_while_live_map_advances() {
        // THE deterministic torn-read proof (single-threaded, no flakiness). The mirror holds a
        // block-(N-1) state where the two pool slots are equal (the block invariant). A validation
        // reads slot0, THEN the advancer applies + publishes block N (both slots move to a new equal
        // value), THEN the SAME validation reads slot1. A frozen snapshot must yield the ORIGINAL
        // block-(N-1) pair — never (old, new).
        let m = StateMirror::new();
        let pool = [0xb1; 20];
        let (s0, s1) = (U256::from(0u64), U256::from(1u64));
        m.apply_batch([(pool, s0, U256::from(10u64)), (pool, s1, U256::from(10u64))]);
        m.publish_certified(100); // block N-1 = 100, both slots = 10
        let f = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 0), 100);
        assert!(f.is_trusted(), "snapshot block == fork pin ⇒ trusted");
        let r0 = f.try_fetch_storage(pool, s0);
        // Advancer ticks mid-validation: live map → block N (=101), both slots = 20, and re-publish.
        m.apply_batch([(pool, s0, U256::from(20u64)), (pool, s1, U256::from(20u64))]);
        m.publish_certified(101);
        let r1 = f.try_fetch_storage(pool, s1);
        assert_eq!(
            (r0, r1),
            (Some(U256::from(10u64)), Some(U256::from(10u64))),
            "the captured snapshot is frozen at block N-1: NEVER a torn (10, 20) cross-block pair"
        );
    }

    #[test]
    fn legacy_live_fetcher_can_observe_a_cross_block_pair() {
        // Documents WHY B1 exists: the legacy live-map fetcher (`new`) reads the mirror directly, so
        // an advance BETWEEN two reads of one validation is visible as a torn pair. This is exactly
        // the state that existed at no block — the bug the published-snapshot path removes.
        let m = StateMirror::new();
        let pool = [0xb2; 20];
        let (s0, s1) = (U256::from(0u64), U256::from(1u64));
        m.apply_batch([(pool, s0, U256::from(10u64)), (pool, s1, U256::from(10u64))]);
        m.set_block(100);
        let f = MirroredStateFetcher::new(&m, mock(0, 0, None, 0), 100); // legacy live-map path
        let r0 = f.try_fetch_storage(pool, s0);
        m.apply_batch([(pool, s0, U256::from(20u64)), (pool, s1, U256::from(20u64))]); // advancer ticks
        let r1 = f.try_fetch_storage(pool, s1);
        assert_eq!(
            (r0, r1),
            (Some(U256::from(10u64)), Some(U256::from(20u64))),
            "the LIVE-map fetcher tears (10, 20) — a cross-block state; the snapshot path (above) does not"
        );
    }

    #[test]
    fn new_published_trust_rules() {
        let m = StateMirror::new();
        let pool = [0xb3; 20];
        m.apply_batch([(pool, U256::from(7u64), U256::from(42u64))]);
        // No snapshot published yet ⇒ nothing to trust ⇒ every read falls to the fallback.
        let f = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 999), 100);
        assert!(!f.is_trusted(), "no published snapshot ⇒ untrusted");
        assert_eq!(f.try_fetch_storage(pool, U256::from(7u64)), Some(U256::from(999u64)));
        // Publish at block 100, but the fork pins at 101 ⇒ block mismatch ⇒ untrusted (RPC).
        m.publish_certified(100);
        let f = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 999), 101);
        assert!(!f.is_trusted(), "snapshot block != fork pin ⇒ untrusted (no cross-block mixing)");
        assert_eq!(f.try_fetch_storage(pool, U256::from(7u64)), Some(U256::from(999u64)));
        // Matching pin ⇒ trusted ⇒ hit served from the snapshot, miss aborts (None), no fallback.
        let f = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 999), 100);
        assert!(f.is_trusted());
        assert_eq!(f.try_fetch_storage(pool, U256::from(7u64)), Some(U256::from(42u64)));
        assert_eq!(f.try_fetch_storage(pool, U256::from(8u64)), None, "trusted miss ⇒ abort");
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn new_published_unhealthy_degrades_to_blocking_even_when_block_matches() {
        // Health is a LIVE gate (not frozen in the snapshot): a mid-flight trip degrades an already
        // block-matched snapshot to blocking-RPC (fail-safe), never a stale snapshot hit or an abort.
        let m = StateMirror::new();
        let pool = [0xb4; 20];
        m.apply_batch([(pool, U256::from(1u64), U256::from(42u64))]);
        m.publish_certified(100);
        m.set_reason(health_reason::GROSS_DIVERGENCE);
        let f = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 777), 100);
        assert!(f.is_trusted(), "trusted flag is block-tag only; health is checked live per read");
        assert_eq!(
            f.try_fetch_storage(pool, U256::from(1u64)),
            Some(U256::from(777u64)),
            "unhealthy ⇒ blocking fallback (NOT the snapshot hit, NOT an abort)"
        );
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn published_arc_is_frozen_against_later_publishes() {
        // A captured snapshot Arc never changes when the advancer publishes a newer block — the
        // foundation of the no-torn-read guarantee (the validator holds this for a whole validation).
        let m = StateMirror::new();
        let pool = [0xb5; 20];
        m.apply_batch([(pool, U256::from(0u64), U256::from(11u64))]);
        m.publish_certified(200);
        let snap = m.published().expect("published");
        assert_eq!(snap.block(), 200);
        assert_eq!(snap.get_storage(pool, U256::from(0u64)), Some(U256::from(11u64)));
        // Advance + republish; the previously captured Arc is immutable and unchanged.
        m.apply_batch([(pool, U256::from(0u64), U256::from(22u64))]);
        m.publish_certified(201);
        assert_eq!(snap.block(), 200, "captured snapshot block is frozen");
        assert_eq!(
            snap.get_storage(pool, U256::from(0u64)),
            Some(U256::from(11u64)),
            "captured snapshot value is frozen against later publishes"
        );
        assert_eq!(m.published().unwrap().block(), 201, "the live published head advanced");
    }

    #[test]
    fn new_ignores_published_snapshot_flag_off_identity() {
        // Flag-off identity: the legacy `new` constructor reads the LIVE map and is oblivious to any
        // published snapshot — byte-identical to pre-B1 (the publish path is opt-in via new_published).
        let m = StateMirror::new();
        let pool = [0xb6; 20];
        m.apply_batch([(pool, U256::from(0u64), U256::from(5u64))]);
        m.set_block(100);
        m.publish_certified(100); // a stale published snapshot with the SAME value
        // Mutate the live map WITHOUT republishing: `new` must see the live value, not the snapshot.
        m.apply_batch([(pool, U256::from(0u64), U256::from(9u64))]);
        let f = MirroredStateFetcher::new(&m, mock(0, 0, None, 0), 100);
        assert_eq!(
            f.try_fetch_storage(pool, U256::from(0u64)),
            Some(U256::from(9u64)),
            "legacy `new` reads the live map (9), never the published snapshot (5)"
        );
    }

    #[test]
    fn concurrent_advance_reader_never_observes_torn_state() {
        // Multi-threaded stress: one advancer thread writes each of K slots one-at-a-time (simulating
        // per-tx application, which leaves the LIVE map transiently torn) then publishes the block; N
        // reader threads capture the published snapshot and assert ALL K slots carry the SAME block
        // marker. If publish were non-atomic or a reader saw the live map, a torn (mixed-marker) read
        // would be observed. It never is.
        use std::sync::Arc;
        let m = Arc::new(StateMirror::new());
        let pool = [0xb7; 20];
        const K: u64 = 8;
        // Seed + publish block 1 so readers always have a consistent snapshot from the start.
        m.apply_batch((0..K).map(|s| (pool, U256::from(s), U256::from(1u64))));
        m.publish_certified(1);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let torn = Arc::new(AtomicU64::new(0));
        let reads = Arc::new(AtomicU64::new(0));

        let mut readers = Vec::new();
        for _ in 0..3 {
            let (m, stop, torn, reads) = (m.clone(), stop.clone(), torn.clone(), reads.clone());
            readers.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(snap) = m.published() {
                        let marker = snap.get_storage(pool, U256::from(0u64)).unwrap();
                        // Every slot must equal slot0's marker — one block's state, no cross-slot mix.
                        for s in 1..K {
                            if snap.get_storage(pool, U256::from(s)) != Some(marker) {
                                torn.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        reads.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }
        // Advancer: flip the block marker across a range, writing slots one-at-a-time then publishing.
        for b in 2u64..20_000 {
            for s in 0..K {
                m.set_storage(pool, U256::from(s), U256::from(b));
            }
            m.publish_certified(b);
        }
        stop.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().unwrap();
        }
        assert_eq!(torn.load(Ordering::Relaxed), 0, "a reader observed a torn cross-slot snapshot");
        assert!(reads.load(Ordering::Relaxed) > 0, "readers must have actually sampled the mirror");
    }

    #[test]
    fn disk_loaded_slots_purge_on_first_retag_to_a_new_pin() {
        // Disk snapshots are canonical at their SAVED block — pin-scoped, not head-fresh. Booting
        // at the same block serves them; the first retag to a different pin drops them.
        let m = StateMirror::new();
        let addr = [0xad; 20];
        m.set_storage(addr, U256::from(1u64), U256::from(42u64));
        m.set_block(200);
        let path = std::env::temp_dir().join("mantis_sm_retag_purge.bin");
        m.save(&path).unwrap();
        let loaded = StateMirror::load(&path).expect("loads");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.get_storage(addr, U256::from(1u64)), Some(U256::from(42u64)));
        loaded.retag_block(200); // same pin → kept
        assert_eq!(loaded.get_storage(addr, U256::from(1u64)), Some(U256::from(42u64)));
        loaded.retag_block(201); // new pin → purged
        assert_eq!(loaded.get_storage(addr, U256::from(1u64)), None);
    }

    #[test]
    fn persist_roundtrip_preserves_block_accounts_storage() {
        let m = StateMirror::new();
        let a1 = [0x11; 20];
        m.set_account(a1, U256::from(1234u64), 9, Some(vec![0xaa, 0xbb]));
        m.set_storage(a1, U256::from(7u64), U256::from(77u64));
        m.set_storage(a1, U256::from(8u64), U256::from(88u64));
        let a2 = [0x22; 20]; // storage-only (no account data primed)
        m.set_storage(a2, U256::from(1u64), U256::from(2u64));
        m.set_block(465686728);

        let path = std::env::temp_dir().join("mantis_sm_roundtrip.bin");
        m.save(&path).unwrap();
        let loaded = StateMirror::load(&path).expect("loads back");
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.block(), 465686728);
        assert_eq!(loaded.account_count(), 2);
        assert_eq!(
            loaded.get_account(a1),
            Some((U256::from(1234u64), 9, Some(vec![0xaa, 0xbb])))
        );
        assert_eq!(
            loaded.get_storage(a1, U256::from(7u64)),
            Some(U256::from(77u64))
        );
        assert_eq!(
            loaded.get_storage(a1, U256::from(8u64)),
            Some(U256::from(88u64))
        );
        // storage-only account: account data still absent after the roundtrip
        assert_eq!(loaded.get_account(a2), None);
        assert_eq!(
            loaded.get_storage(a2, U256::from(1u64)),
            Some(U256::from(2u64))
        );
    }

    #[test]
    fn load_rejects_corrupt_and_missing() {
        let path = std::env::temp_dir().join("mantis_sm_corrupt.bin");
        let m = StateMirror::new();
        m.set_account([0x33; 20], U256::from(5u64), 1, None);
        m.set_block(10);
        m.save(&path).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0xff; // corrupt last body byte → checksum mismatch
        std::fs::write(&path, &bytes).unwrap();
        assert!(StateMirror::load(&path).is_none());
        let _ = std::fs::remove_file(&path);
        assert!(StateMirror::load(&std::env::temp_dir().join("mantis_sm_nope.bin")).is_none());
    }

    /// Manual latency bench (RELEASE only):
    /// `cargo test -p engine --release state_mirror::tests::mirror_hit_latency -- --ignored --nocapture`
    ///
    /// Measures the per-read cost of a mirror hit — the value that replaces a ~30–80 ms RPC
    /// round-trip on every account/slot the REVM validator touches.
    #[test]
    #[ignore]
    fn mirror_hit_latency() {
        use std::time::Instant;
        let mirror = StateMirror::new();
        // Prime a realistic snapshot working set: ~4k pools/accounts, ~16 hot slots each.
        let n_accounts = 4096usize;
        let slots_per = 16usize;
        for i in 0..n_accounts {
            let mut addr = [0u8; 20];
            addr[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            mirror.set_account(addr, U256::from(i as u64), 1, Some(vec![0u8; 64]));
            for s in 0..slots_per {
                mirror.set_storage(addr, U256::from(s as u64), U256::from((i * s) as u64));
            }
        }
        mirror.set_block(1);
        let f = MirroredStateFetcher::new(&mirror, mock(0, 0, None, 0), 1);
        let iters = 2_000_000u64;
        let t = Instant::now();
        let mut acc = U256::ZERO;
        for k in 0..iters {
            let i = (k as usize) % n_accounts;
            let s = (k as usize) % slots_per;
            let mut addr = [0u8; 20];
            addr[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            acc = acc.wrapping_add(f.fetch_storage(addr, U256::from(s as u64)));
        }
        let el = t.elapsed();
        // Every read was a hit — zero fallback RPC.
        assert_eq!(f.fallback.storage_calls.load(Ordering::Relaxed), 0);
        let ns = el.as_nanos() as f64 / iters as f64;
        println!(
            "mirror storage-hit: {:.1} ns/read ({} reads in {:?}); \
             RPC-miss baseline ~30 ms => ~{:.0}e6x faster per read; acc_guard={}",
            ns,
            iters,
            el,
            30_000_000.0 / ns,
            acc
        );
    }

    /// Manual PB-STATE bench (RELEASE only):
    /// `cargo test -p engine --release state_mirror::tests::persist_load_latency -- --ignored --nocapture`
    ///
    /// Saves + reloads a snapshot-sized mirror to substantiate "load instantly at boot vs re-fetch
    /// ~3.6k pools over RPC (~8 min)".
    #[test]
    #[ignore]
    fn persist_load_latency() {
        use std::time::Instant;
        let m = StateMirror::new();
        let n_accounts = 4096usize;
        let slots_per = 16usize;
        for i in 0..n_accounts {
            let mut addr = [0u8; 20];
            addr[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            m.set_account(addr, U256::from(i as u64), 1, Some(vec![0u8; 64]));
            for s in 0..slots_per {
                m.set_storage(addr, U256::from(s as u64), U256::from((i * s) as u64));
            }
        }
        m.set_block(465686728);
        let path = std::env::temp_dir().join("mantis_sm_bench.bin");
        let t0 = Instant::now();
        m.save(&path).unwrap();
        let save_ms = t0.elapsed().as_secs_f64() * 1e3;
        let bytes = std::fs::metadata(&path).unwrap().len();
        let t1 = Instant::now();
        let loaded = StateMirror::load(&path).expect("loads");
        let load_ms = t1.elapsed().as_secs_f64() * 1e3;
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.account_count(), n_accounts);
        assert_eq!(loaded.block(), 465686728);
        println!(
            "PB-STATE: {} accounts × {} slots = {:.2} MiB on disk; save {:.2} ms, load {:.2} ms \
             (vs ~8 min RPC snapshot)",
            n_accounts,
            slots_per,
            bytes as f64 / (1024.0 * 1024.0),
            save_ms,
            load_ms
        );
    }

    // ── [WP2 · Fix C] reason-owned health + certified snapshots ──────────────────────────────────

    #[test]
    fn health_reasons_are_independently_owned() {
        // WP2 RED-2 + RED-7: each reason is set/cleared independently. Clearing one (a proved-gone
        // queue gap, or the whole reconcile-heal group) can NEVER erase another (unresolved proof
        // debt, or the sticky full-resync latch).
        let m = StateMirror::new();
        assert!(m.is_healthy(), "no reason ⇒ healthy");
        m.set_reason(
            health_reason::QUEUE_GAP
                | health_reason::PROOF_UNRESOLVED
                | health_reason::FULL_RESYNC_REQUIRED,
        );
        assert!(!m.is_healthy());
        assert!(m.has_reason(health_reason::PROOF_UNRESOLVED));
        // RED-2: clearing the queue-gap reason cannot clear the proof debt.
        m.clear_reason(health_reason::QUEUE_GAP);
        assert!(!m.has_reason(health_reason::QUEUE_GAP), "queue gap cleared");
        assert!(
            m.has_reason(health_reason::PROOF_UNRESOLVED),
            "proof debt survives an unrelated clear"
        );
        // RED-7: the reconcile-heal group clear must NOT clear the sticky full-resync latch.
        m.set_reason(health_reason::GROSS_DIVERGENCE | health_reason::REORG_RECOVERY);
        m.clear_reason(
            health_reason::QUEUE_GAP
                | health_reason::GROSS_DIVERGENCE
                | health_reason::PROOF_UNRESOLVED
                | health_reason::REORG_RECOVERY,
        );
        assert!(!m.has_reason(health_reason::PROOF_UNRESOLVED));
        assert!(
            m.has_reason(health_reason::FULL_RESYNC_REQUIRED),
            "the sticky full-resync latch survives the heal-group clear"
        );
        assert!(!m.is_healthy(), "still untrusted while the sticky reason is set");
        // Only clearing the sticky reason (explicit resync tooling) restores trust.
        m.clear_reason(health_reason::FULL_RESYNC_REQUIRED);
        assert!(m.is_healthy());
    }

    #[test]
    fn uncertified_snapshot_is_never_trusted() {
        // WP2 RED-5: a snapshot frozen by an UNcertified path is never trusted — not at a matching
        // block, and not because the mirror is healthy (no other reason set).
        let m = StateMirror::new();
        m.apply_batch([([0xb4; 20], U256::from(1u64), U256::from(42u64))]);
        m.publish(100); // UNcertified publish (did not pass the close publish gate)
        assert!(m.is_healthy(), "no reason set");
        let f = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 777), 100);
        assert!(!f.is_trusted(), "an uncertified snapshot is never trusted, even at a matching block");
        // A CERTIFIED publish at the same block IS trusted.
        m.publish_certified(100);
        let f2 = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 777), 100);
        assert!(f2.is_trusted(), "a certified snapshot at a matching block is trusted");
    }

    #[test]
    fn certified_publish_stamps_monotonic_generation() {
        // WP2 RED-6: each certified publish stamps a monotonically increasing immutable generation and
        // the exact block is trusted.
        let m = StateMirror::new();
        m.apply_batch([([0xb5; 20], U256::from(1u64), U256::from(7u64))]);
        m.publish_certified(100);
        let s1 = m.published().unwrap();
        assert!(s1.certified() && s1.block() == 100);
        let gen1 = s1.generation();
        m.publish_certified(100);
        let s2 = m.published().unwrap();
        assert!(s2.generation() > gen1, "generation is monotonic across certified publishes");
        let f = MirroredStateFetcher::new_published(&m, mock(0, 0, None, 0), 100);
        assert!(f.is_trusted(), "a certified exact-block snapshot is trusted");
    }
}
